//! rusqlite 基建(方案 #14 R4.1,决策 D3)。
//!
//! **单写者 + 多读者**模型,与 Python 侧同构但按 Rust 惯例重排:
//!
//! - **专用写线程**:一个命名线程独占 `Connection`,通过 mpsc 队列接收
//!   闭包任务(`write` / `write_autocommit`),用 oneshot 回传结果。
//!   整库只有一个写连接 = 天然单写者,不存在「两个连接抢写锁」的自我抖动;
//!   这也是 R4.13「日志写入 P99 < 2ms(写线程隔离)」的隔离手段。
//! - **读者 spawn_blocking**:每次读在 blocking 池上开一条只跑查询的连接
//!   (`PRAGMA query_only=ON`),WAL 下读者与写者互不阻塞。
//! - **PRAGMA 与 Python 一致**:`journal_mode=WAL`、`foreign_keys=ON`、
//!   `busy_timeout=10000`(Python `sqlite3.connect(timeout=10)` 的等价物)。
//!   WAL 是库文件的持久属性,双方对同一个 `config.sqlite` 的判定一致。
//!
//! 事务边界镜像 Python 的 `_session()`:任务体隐式 `BEGIN`,正常退出
//! `COMMIT`,出错 `ROLLBACK` 并把错误冒泡给调用方。`VACUUM` /
//! `wal_checkpoint` 不能开在事务里,走 [`DbEngine::write_autocommit`]。
//!
//! 停机不丢(R4.11 的前提):[`DbEngine::close`] 先丢弃发送端让写线程
//! 排空队列再退出,已 `await` 的写在其完成前调用方不会返回。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{mpsc, oneshot};

use wr_app::ports::PortError;

/// 写线程接收的任务:在写连接上执行并把结果回传给 `oneshot` 发送端。
type Job = Box<dyn FnOnce(&Connection) + Send + 'static>;

/// 写队列深度。队列满时 `write()` 异步等待(背压),而不是丢任务。
const WRITE_QUEUE_DEPTH: usize = 512;

/// 共享的 SQLite 访问引擎。克隆廉价(内部 `Arc`),`SqliteStore` 与各
/// 端口实现都持有它的副本。
#[derive(Debug, Clone)]
pub struct DbEngine {
    inner: Arc<EngineInner>,
}

#[derive(Debug)]
struct EngineInner {
    path: PathBuf,
    sender: tokio::sync::Mutex<Option<mpsc::Sender<Job>>>,
    writer: tokio::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    closed: AtomicBool,
}

impl DbEngine {
    /// 打开(必要时创建)`<data_dir>/config.sqlite` 并启动写线程。
    ///
    /// 写连接在这里同步打开:失败直接返回错误,而不是等第一个任务进来
    /// 才发现线程没有连接可用。
    pub async fn open(path: &Path) -> Result<Self, PortError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                PortError::storage(format!("create data dir {} failed: {e}", parent.display()))
            })?;
        }
        let write_conn = open_write_connection(path)?;
        let path_buf = path.to_path_buf();
        let (sender, mut receiver) = mpsc::channel::<Job>(WRITE_QUEUE_DEPTH);
        let handle = std::thread::Builder::new()
            .name("wr-sqlite-writer".to_owned())
            .spawn(move || {
                let mut conn = Some(write_conn);
                // 发送端全部丢弃后 blocking_recv 返回 None:队列里已排队的
                // 任务先跑完(排空语义由 mpsc 保证),然后落盘 checkpoint
                // 并关闭连接 —— 停机不丢写。
                while let Some(job) = receiver.blocking_recv() {
                    if let Some(connection) = conn.as_ref() {
                        job(connection);
                    }
                }
                if let Some(connection) = conn.take() {
                    let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
                }
            })
            .map_err(|e| PortError::storage(format!("spawn sqlite writer thread failed: {e}")))?;
        drop(path_buf); // 路径已在写连接中持有,这里只保留副本给读者。
        Ok(Self {
            inner: Arc::new(EngineInner {
                path: path.to_path_buf(),
                sender: tokio::sync::Mutex::new(Some(sender)),
                writer: tokio::sync::Mutex::new(Some(handle)),
                closed: AtomicBool::new(false),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Relaxed)
    }

    /// 在写线程上执行一个事务型任务(隐式 BEGIN / COMMIT / ROLLBACK)。
    pub async fn write<T, F>(&self, f: F) -> Result<T, PortError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, PortError> + Send + 'static,
    {
        self.dispatch(f, true).await
    }

    /// 在写线程上执行一个自动提交任务(不开事务):`VACUUM`、
    /// `PRAGMA wal_checkpoint` 这类不能在事务里运行的语句。
    pub async fn write_autocommit<T, F>(&self, f: F) -> Result<T, PortError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, PortError> + Send + 'static,
    {
        self.dispatch(f, false).await
    }

    async fn dispatch<T, F>(&self, f: F, in_transaction: bool) -> Result<T, PortError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, PortError> + Send + 'static,
    {
        if self.inner.closed.load(Ordering::Relaxed) {
            return Err(PortError::storage("SqliteStore has been closed"));
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        // FnOnce 闭包只能调一次:塞进 Option,写线程取出后必然消费。
        let mut task = Some(f);
        let job: Job = Box::new(move |connection: &Connection| {
            let Some(function) = task.take() else {
                return;
            };
            let outcome = if in_transaction {
                run_in_transaction(connection, function)
            } else {
                function(connection)
            };
            let _ = reply_tx.send(outcome);
        });
        {
            let guard = self.inner.sender.lock().await;
            let Some(sender) = guard.as_ref() else {
                return Err(PortError::storage("SqliteStore has been closed"));
            };
            sender.send(job).await.map_err(|_| {
                if self.inner.closed.load(Ordering::Relaxed) {
                    PortError::storage("SqliteStore has been closed")
                } else {
                    PortError::storage("sqlite writer thread is no longer running")
                }
            })?;
        }
        reply_rx
            .await
            .map_err(|_| PortError::storage("sqlite writer thread dropped the task (panicked?)"))?
    }

    /// 在 blocking 池上执行一次只读查询。
    pub async fn read<T, F>(&self, f: F) -> Result<T, PortError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, PortError> + Send + 'static,
    {
        let path = self.inner.path.clone();
        tokio::task::spawn_blocking(move || match open_read_connection(&path) {
            Ok(connection) => f(&connection),
            Err(error) => Err(error),
        })
        .await
        .map_err(|e| PortError::storage(format!("sqlite read task failed to join: {e}")))?
    }

    /// 排空写队列并停掉写线程(幂等)。
    pub async fn close(&self) {
        let sender = self.inner.sender.lock().await.take();
        self.inner.closed.store(true, Ordering::SeqCst);
        drop(sender);
        let handle = self.inner.writer.lock().await.take();
        if let Some(handle) = handle {
            let _ = tokio::task::spawn_blocking(move || handle.join()).await;
        }
    }

    /// 同步关闭:供非 async 上下文(测试夹具)使用。
    pub fn close_blocking(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.inner.sender.try_lock() {
            guard.take();
        }
        if let Ok(mut guard) = self.inner.writer.try_lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}

/// 事务包装:出错时回滚并冒泡(镜像 Python `_session`)。
fn run_in_transaction<T, F>(connection: &Connection, f: F) -> Result<T, PortError>
where
    F: FnOnce(&Connection) -> Result<T, PortError>,
{
    connection
        .execute_batch("BEGIN")
        .map_err(|e| PortError::storage(format!("BEGIN failed: {e}")))?;
    match f(connection) {
        Ok(value) => match connection.execute_batch("COMMIT") {
            Ok(()) => Ok(value),
            Err(e) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(PortError::storage(format!("COMMIT failed: {e}")))
            }
        },
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn apply_pragmas(connection: &Connection) -> Result<(), rusqlite::Error> {
    // 顺序与 Python `_connect` 一致:foreign_keys → journal_mode → busy_timeout。
    // 20261017_02 批4:synchronous=NORMAL(WAL 官方推荐组合)——提交不再
    // 每次 fsync WAL(仅 checkpoint 时),慢盘上每批落库省 10-100ms。
    // 权衡:断电(非应用崩溃)可能丢最后几条已应答事务;WAL 内一致性不受
    // 影响,Python 侧读写同一文件也不受影响(synchronous 是连接级)。
    connection.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=10000;",
    )
}

fn open_write_connection(path: &Path) -> Result<Connection, PortError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| PortError::storage(format!("open {} failed: {e}", path.display())))?;
    apply_pragmas(&connection).map_err(|e| PortError::storage(format!("pragma failed: {e}")))?;
    Ok(connection)
}

/// 读者连接:`READ_WRITE` 打开(只读连接在 WAL 下需要能建 `-shm`,
/// 换 READ_WRITE 免掉这一环境依赖),再用 `query_only` 从 SQLite 层面
/// 禁止任何写。
fn open_read_connection(path: &Path) -> Result<Connection, PortError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| PortError::storage(format!("open {} failed: {e}", path.display())))?;
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA query_only=ON; PRAGMA busy_timeout=10000;")
        .map_err(|e| PortError::storage(format!("pragma failed: {e}")))?;
    Ok(connection)
}

/// UTC 当前时刻的 ISO-8601 秒精度串(镜像 `sqlite_base.iso_now()`:
/// `datetime.now(utc).replace(microsecond=0).isoformat()` 再把
/// `+00:00` 换成 `Z`)。
pub fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 当前时刻的 epoch 毫秒。**Rust 侧统一时钟基**:
/// - Python `time.time()*1000`(凭据池冷却)与之一致;
/// - Python `time.monotonic()*1000`(熔断开闸时刻、幂等条目过期)是开机起算
///   的另一个域,Rust 改用 epoch 毫秒。跨运行时读写同一数据目录时的归一化
///   见 [`normalize_future_ms`] 与 14 号文档 R4 完成记录。
pub fn wall_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 把「来自其它时钟基」的未来时刻归一化到本运行时可信区间:
/// 只有落在 `(now, now + horizon]` 内的值才当作真实的未来时刻保留,
/// 其余(远古或遥远未来)按已到期返回 `0`。
///
/// 存在意义:Python 持久化的是 `time.monotonic()*1000`(开机起算),
/// Rust 持久化的是 epoch 毫秒(1970 起算)。两套数值不可直接比较,而
/// 迁移期同一个数据目录可能被双方读写。归一化的取舍是「宁可多放一次
/// 试探请求,不可把 provider 永久锁死」。
pub fn normalize_future_ms(loaded: i64, now: i64, horizon: i64) -> i64 {
    if loaded > now && loaded <= now.saturating_add(horizon) {
        loaded
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqlContext as _;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wr-r4-engine-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.sqlite")
    }

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let path = temp_db("roundtrip");
        let engine = DbEngine::open(&path).await.unwrap();
        engine
            .write(|conn| {
                conn.execute(
                    "CREATE TABLE IF NOT EXISTS t (a INTEGER PRIMARY KEY, b TEXT NOT NULL)",
                    [],
                )
                .context("create t")?;
                conn.execute("INSERT INTO t(b) VALUES('x')", [])
                    .context("insert x")?;
                Ok(())
            })
            .await
            .unwrap();
        let count: i64 = engine
            .read(|conn| {
                conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
                    .context("count t")
            })
            .await
            .unwrap();
        assert_eq!(count, 1);
        engine.close().await;
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn error_rolls_back_the_transaction() {
        let path = temp_db("rollback");
        let engine = DbEngine::open(&path).await.unwrap();
        engine
            .write(|conn| {
                conn.execute_batch("CREATE TABLE t (a INTEGER)")
                    .context("create t")
            })
            .await
            .unwrap();
        let failed: Result<(), PortError> = engine
            .write(|conn| {
                conn.execute("INSERT INTO t(a) VALUES(1)", [])
                    .context("insert 1")?;
                Err(PortError::storage("boom"))
            })
            .await;
        assert!(failed.is_err());
        let count: i64 = engine
            .read(|conn| {
                conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
                    .context("count t")
            })
            .await
            .unwrap();
        assert_eq!(count, 0, "出错必须回滚,不留半写");
        engine.close().await;
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn readers_are_blocked_from_writing() {
        let path = temp_db("query_only");
        let engine = DbEngine::open(&path).await.unwrap();
        engine
            .write(|conn| {
                conn.execute_batch("CREATE TABLE t (a INTEGER)")
                    .context("create t")
            })
            .await
            .unwrap();
        let attempted = engine
            .read(|conn| {
                conn.execute("INSERT INTO t(a) VALUES(1)", [])
                    .context("insert into reader")
            })
            .await;
        assert!(
            attempted.is_err(),
            "读者连接必须被 query_only 挡住写(C5 单写者)"
        );
        engine.close().await;
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn close_drains_queued_writes() {
        let path = temp_db("drain");
        let engine = DbEngine::open(&path).await.unwrap();
        engine
            .write(|conn| {
                conn.execute_batch("CREATE TABLE t (a INTEGER)")
                    .context("create t")
            })
            .await
            .unwrap();
        engine.close().await;
        assert!(engine.is_closed());
        let after = engine
            .write(|conn| {
                conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get::<_, i64>(0))
                    .context("count t")
            })
            .await;
        assert!(after.is_err(), "关闭后写入应报错而不是静默丢弃");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn normalize_future_ms_only_keeps_plausible_horizon() {
        // Python 写的 monotonic 值(开机起算,远小于 epoch 毫秒)→ 视为已到期。
        assert_eq!(
            normalize_future_ms(36_000_000, 1_700_000_000_000, 30_000),
            0
        );
        // 本运行时写入、仍在冷却中 → 原样保留。
        assert_eq!(
            normalize_future_ms(1_700_000_005_000, 1_700_000_000_000, 30_000),
            1_700_000_005_000
        );
        // 超出可信区间的遥远未来(Rust 行被 Python 时钟基读到时的形态)→ 归零。
        assert_eq!(
            normalize_future_ms(1_700_000_000_000_000, 1_700_000_000_000, 30_000),
            0
        );
        // 过去值同样归零。
        assert_eq!(normalize_future_ms(1, 1_700_000_000_000, 30_000), 0);
    }
}
