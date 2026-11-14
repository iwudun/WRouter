//! 配置实体(镜像 Python `domain/configuration/entities.py`)。
//!
//! R1 阶段:C9 兼容的 JSON 读取结构(`AppConfig` 及其可靠性子块)暂驻
//! `wr-infra`,因为 Python 侧读取同样落在基础设施持久层;R3/R4 将实体
//! 迁入本模块并保持字段一一对应。
//!
//! 本模块先落地与 IO 无关的纯函数:`normalize_theme`(对应 Python
//! `entities.py::_normalize_theme`,与前端 `normalizeThemePreference`
//! 保持一致)。

/// 把任意主题值收敛为三个合法 token:`system` / `light` / `dark`。
///
/// Python 语义:入参非字符串(或缺失、空串)一律回退 `"system"`;
/// 字符串先 `strip().lower()` 再匹配,未识别值(如旧实验的 `auto`)
/// 同样回退 `"system"`。`s` 传 `None` 表示两个候选键都缺失/为假值。
pub fn normalize_theme(value: Option<&str>) -> &'static str {
    match value {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "light" => "light",
            "dark" => "dark",
            "system" => "system",
            _ => "system",
        },
        None => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_theme;

    #[test]
    fn keeps_known_tokens() {
        assert_eq!(normalize_theme(Some("light")), "light");
        assert_eq!(normalize_theme(Some("dark")), "dark");
        assert_eq!(normalize_theme(Some("system")), "system");
    }

    #[test]
    fn normalizes_case_and_whitespace() {
        assert_eq!(normalize_theme(Some(" Dark ")), "dark");
        assert_eq!(normalize_theme(Some("LIGHT")), "light");
    }

    #[test]
    fn unknown_and_missing_fall_back_to_system() {
        // 旧实验 token "auto" 与任意未识别值都回退 system。
        assert_eq!(normalize_theme(Some("auto")), "system");
        assert_eq!(normalize_theme(Some("")), "system");
        assert_eq!(normalize_theme(None), "system");
    }
}
