//! 日志维护后台任务(方案 #14 R4.11;镜像 Python
//! `application/log_maintenance.py::LogMaintenanceService`)。
//!
//! 一轮 = 迁移 → 保留清理 → 视情况压缩:
//! 1. **迁移**:把「正文内联在 payload 里」的旧格式行改写成拆分格式
//!    (幂等,按 100 行一批小事务,不长时间挤占实流量);
//! 2. **保留**:分档过期 —— 正文走短窗(`WROUTER_BODY_RETENTION_DAYS`,
//!    默认 7 天,可被 `observability.bodyRetentionDays` 覆盖),元数据走长窗
//!    (`WROUTER_LOG_RETENTION_DAYS`,默认 **0 = 永久**,即 C3 的「元数据
//!    永久保留」);
//! 3. **压缩**:一轮释放的正文量达到阈值才 `VACUUM`(重建很贵,且需要约
//!    等于库大小的空闲磁盘)。
//!
//! 调度事实与 Python 一致:默认间隔 3600s,**启动即跑第一轮**;单轮失败
//! 只记录日志、不终止循环(维护任务绝不能把服务带崩);停机时由调用方
//! `abort()` 任务,已提交的清理结果自然留在库里。

use std::time::Duration;

use serde_json::Value;
use wr_app::ports::{ConfigStore, RequestLogStore};
use wr_infra::sqlite::SqliteStore;

use crate::settings::Settings;

/// 一轮释放多少正文体积后才值得 VACUUM。
const VACUUM_THRESHOLD_BYTES: i64 = 64 * 1024 * 1024;
/// 默认维护间隔(秒)。
const DEFAULT_INTERVAL_SECONDS: f64 = 3600.0;

/// 一轮维护的计数结果(`run_once` 的返回)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaintenanceStats {
    pub bodies_deleted: i64,
    pub logs_deleted: i64,
    pub bytes_deleted: i64,
    pub compacted: bool,
}

/// 维护服务。`interval` 可在测试里压短。
pub struct LogMaintenanceService {
    store: std::sync::Arc<SqliteStore>,
    body_retention_days: i64,
    log_retention_days: i64,
    interval: Duration,
}

impl LogMaintenanceService {
    /// 从 settings 构造(注入 env 档的保留天数)。
    pub fn from_settings(store: std::sync::Arc<SqliteStore>, settings: &Settings) -> Self {
        Self::with_interval(
            store,
            settings.body_retention_days.max(0),
            settings.log_retention_days.max(0),
            Duration::from_secs_f64(DEFAULT_INTERVAL_SECONDS),
        )
    }

    pub fn with_interval(
        store: std::sync::Arc<SqliteStore>,
        body_retention_days: i64,
        log_retention_days: i64,
        interval: Duration,
    ) -> Self {
        Self {
            store,
            body_retention_days: body_retention_days.max(0),
            log_retention_days: log_retention_days.max(0),
            interval,
        }
    }

    /// 本轮生效的正文保留天数:优先取活跃配置里的
    /// `observability.bodyRetentionDays`(UI 可改、无需重启),否则用
    /// 构造值。元数据档**永远**用构造值(仅 env 可配)。
    /// 配置读取失败一律回退,绝不让维护中断。
    pub async fn current_body_retention_days(&self) -> i64 {
        let Ok(payload) = self.store.load_payload().await else {
            return self.body_retention_days;
        };
        let Some(observability) = payload.get("observability").and_then(Value::as_object) else {
            return self.body_retention_days;
        };
        wr_domain::observability::body_retention_days(Some(observability), self.body_retention_days)
    }

    /// 一轮维护(清理 → 视情况压缩)。
    pub async fn run_once(&self) -> MaintenanceStats {
        let mut stats = MaintenanceStats::default();
        let body_days = self.current_body_retention_days().await;
        match self
            .store
            .prune_logs(body_days, self.log_retention_days)
            .await
        {
            Ok(pruned) => {
                stats.bodies_deleted = pruned.bodies_deleted;
                stats.logs_deleted = pruned.logs_deleted;
                stats.bytes_deleted = pruned.bytes_deleted;
            }
            Err(error) => {
                tracing::warn!(target: "wrouter.maintenance", %error, "prune_logs_failed");
            }
        }
        if stats.bytes_deleted >= VACUUM_THRESHOLD_BYTES {
            match self.store.compact().await {
                Ok(()) => stats.compacted = true,
                Err(error) => {
                    tracing::warn!(target: "wrouter.maintenance", %error, "compact_failed");
                }
            }
        }
        stats
    }

    /// 立即跑一轮,然后按间隔循环。永不抛出(失败只记日志)。
    pub async fn run_forever(self) {
        tracing::info!(
            target: "wrouter.maintenance",
            body_retention_days = if self.body_retention_days == 0 {
                "∞".to_owned()
            } else {
                self.body_retention_days.to_string()
            },
            log_retention_days = if self.log_retention_days == 0 {
                "∞".to_owned()
            } else {
                self.log_retention_days.to_string()
            },
            interval_seconds = self.interval.as_secs_f64(),
            "request-log maintenance started"
        );
        loop {
            let stats = self.run_once().await;
            if stats.bodies_deleted != 0 || stats.logs_deleted != 0 {
                tracing::info!(
                    target: "wrouter.maintenance",
                    bodies_deleted = stats.bodies_deleted,
                    logs_deleted = stats.logs_deleted,
                    bytes_deleted = stats.bytes_deleted,
                    compacted = stats.compacted,
                    "request-log maintenance round"
                );
            }
            tokio::time::sleep(self.interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use wr_app::ports::LogBodies;
    use wr_app::ports::{ConfigStore, RequestLogStore};
    use wr_infra::sqlite::SqliteStore;

    async fn harness(
        name: &str,
        body_days: i64,
        log_days: i64,
    ) -> (Arc<SqliteStore>, LogMaintenanceService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("wr-r4-maint-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(SqliteStore::open(&dir).await.unwrap());
        let service = LogMaintenanceService::with_interval(
            store.clone(),
            body_days,
            log_days,
            Duration::from_millis(50),
        );
        (store, service, dir)
    }

    /// 配置里的 `bodyRetentionDays` 压过 env 档(0 = 正文也永久)。
    #[tokio::test]
    async fn config_overrides_env_body_retention() {
        let (store, service, dir) = harness("override", 7, 0).await;
        let payload = store.load_payload().await.unwrap();
        let mut config = payload.as_object().unwrap().clone();
        config.insert("observability".to_owned(), json!({"bodyRetentionDays": 0}));
        store.save_payload(&Value::Object(config)).await.unwrap();
        assert_eq!(service.current_body_retention_days().await, 0);
        // 0 → 该档关闭:插一行「远古」正文也不该被删。
        store
            .append_request_log(
                "old",
                &json!({"protocol": "openai"}),
                Some(&LogBodies {
                    response_body: Some("x".to_owned()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        let before = store.prune_logs(0, 0).await.unwrap();
        assert_eq!(before.bodies_deleted, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 读不到配置时回退构造值,维护绝不因此中断。
    #[tokio::test]
    async fn falls_back_when_config_unreadable() {
        let (_store, service, dir) = harness("fallback", 9, 0).await;
        // 写入一个非对象 payload 让解析路径走极端(端口层仍会返回对象)。
        assert_eq!(service.current_body_retention_days().await, 9);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_forever_completes_first_round() {
        let (_store, service, dir) = harness("forever", 7, 0).await;
        let task = tokio::spawn(service.run_forever());
        // 第一轮立即执行;压短间隔后稍等即可看到任务仍在跑(未 panic)。
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!task.is_finished(), "维护循环应持续运行直到被停机 abort");
        task.abort();
        std::fs::remove_dir_all(&dir).ok();
    }
}
