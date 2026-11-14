//! 节省动作解析(20260905_03 §3;20261101_02 T3-B 修订)。
//!
//! 规则域下线后触发链只剩**全局一层**:所有动作取自 [`SavingsConfig`]
//! (压缩 mode、响应缓存 enabled、粘性/钉扎全局开关)。粘性/钉扎原只能
//! 由规则 `stickySession`/`prefixPin` 动作触发(全局无开关),现提升为
//! 全局开关,默认关;两张 TTL 表与健康门/成功回写机制原样保留。

use wr_domain::routing::contracts::RouterCompressionTier;
use wr_infra::config::SavingsConfig;

/// 解析后的生效动作(选路/出站点的事实来源;字段均为具体值,无 None)。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSavings {
    /// 压缩档位(压缩管线消费)。
    pub compression_tier: RouterCompressionTier,
    pub sticky_session: bool,
    pub prefix_pin: bool,
    /// 响应缓存门。
    pub response_cache: bool,
}

impl Default for ResolvedSavings {
    fn default() -> Self {
        Self {
            compression_tier: RouterCompressionTier::Off,
            sticky_session: false,
            prefix_pin: false,
            response_cache: false,
        }
    }
}

/// 全局解析(20261101_02 T3-B:无规则层,`SavingsConfig` 即全部来源)。
pub fn resolve_savings(global: &SavingsConfig) -> ResolvedSavings {
    ResolvedSavings {
        // 压缩:全局 mode;未知/缺失一律 off(off 即总开关,方案 §7.2)。
        compression_tier: match global.compression.mode.as_str() {
            "auto" => RouterCompressionTier::Auto,
            "full" => RouterCompressionTier::Full,
            _ => RouterCompressionTier::Off,
        },
        // 粘性/钉扎:全局开关(规则域下线后的新契约;默认关)。
        sticky_session: global.sticky.enabled,
        prefix_pin: global.prefix_pin.enabled,
        // 响应缓存:全局 enabled。
        response_cache: global.response_cache.enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wr_infra::config::{
        SavingsCompressionConfig, SavingsPrefixPinConfig, SavingsResponseCacheConfig,
        SavingsStickyConfig,
    };

    #[test]
    fn default_global_resolves_to_conservative_off() {
        // 全默认:压缩 off、粘性/钉扎关、响应缓存关。
        assert_eq!(
            resolve_savings(&SavingsConfig::default()),
            ResolvedSavings::default()
        );
    }

    #[test]
    fn global_switches_drive_actions() {
        let global = SavingsConfig {
            compression: SavingsCompressionConfig {
                mode: "full".to_owned(),
                threshold_tokens: 1_000,
            },
            response_cache: SavingsResponseCacheConfig {
                enabled: true,
                ..Default::default()
            },
            sticky: SavingsStickyConfig {
                enabled: true,
                ttl_secs: 900,
                // 阶段五 C-1:冷判定窗口(此处不影响开关解析)。
                cache_cold_secs: 300,
            },
            prefix_pin: SavingsPrefixPinConfig { enabled: true },
        };
        let resolved = resolve_savings(&global);
        assert_eq!(resolved.compression_tier, RouterCompressionTier::Full);
        assert!(resolved.response_cache);
        assert!(resolved.sticky_session);
        assert!(resolved.prefix_pin);
        // mode 坏值 → off(off 即总开关)。
        let mut global = global;
        global.compression.mode = "bogus".to_owned();
        assert_eq!(
            resolve_savings(&global).compression_tier,
            RouterCompressionTier::Off
        );
    }
}
