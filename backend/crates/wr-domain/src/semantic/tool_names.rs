//! 工具名双向可逆清洗(2026-10-14 审计缺陷,P1-3)。
//!
//! 上游名册限制比 openai 家族严:anthropic `^[a-zA-Z0-9_-]{1,128}$`、
//! gemini 首字符须字母/下划线且 ≤63——MCP 名含 `.`/`:` 直发即 400。
//! 不用每请求 forward/reverse map(其响应侧需要请求上下文
//! 穿线,WRouter 的解码是纯函数),改用**确定性转义编码**:
//! 先把字面 `_` 转义为 `_5F_`,再把每个非法字符编为 `_<2位十六进制大写>_`。
//! 解码单遍左到右匹配 `_HH_`,因原名中的 `_` 已全部先行转义,编码形态
//! 的 `_HH_` 只可能来自本编码,无歧义、无需映射表;合法名原样直通
//! (99% 名字零开销,响应侧无条件解码也安全)。
//! 已知有损边角:转义后超长只做截断(极长 MCP 名),截断名下轮稳定。

/// anthropic 工具名上限(按官方文档)。
const ANTHROPIC_MAX: usize = 128;
/// gemini functionDeclarations 名上限(取文档口径的保守值)。
const GEMINI_MAX: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Anthropic,
    Gemini,
}

fn charset_allows(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// 判断名字是否已符合目标名册(符合则原样直通)。
fn is_compliant(profile: Profile, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // 含 `_` 的名字不得直通:`_HH_` 形态与编码歧义,必须全量转义。
    if name.contains('_') {
        return false;
    }
    if matches!(profile, Profile::Gemini)
        && !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return false;
    }
    name.chars().all(|c| charset_allows(c))
}

/// 原名 → 上游可合法承载的编码名。合法名直通;否则全量转义。
pub fn sanitize(name: &str, profile: Profile) -> String {
    if is_compliant(profile, name) {
        return name.to_owned();
    }
    let mut out = String::with_capacity(name.len() * 2);
    if matches!(profile, Profile::Gemini)
        && !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        // 数字/非法字符开头(gemini 不允许):前置下划线补合法首字符。
        out.push('_');
    }
    for c in name.chars() {
        if c == '_' {
            out.push_str("_5F_");
        } else if charset_allows(c) {
            out.push(c);
        } else if (c as u32) <= 0xFF {
            out.push_str(&format!("_{:02X}_", c as u32));
        } else {
            out.push_str(&format!("_{:04X}_", c as u32));
        }
    }
    let max = match profile {
        Profile::Anthropic => ANTHROPIC_MAX,
        Profile::Gemini => GEMINI_MAX,
    };
    if out.len() > max {
        out.truncate(max);
    }
    out
}

/// 上游回流的工具名 → 原名。单遍解码 `_HH_`/`_HHHH_`;非编码形态原样
/// 通过,因此对未经本编码的名字(合规直通、第三方原生名)无条件调用也
/// 安全。原名中的 `_` 已先行转义,故线上的 `_HH_` 只可能来自本编码。
pub fn desanitize(name: &str) -> String {
    if !name.contains('_') {
        return name.to_owned();
    }
    let bytes = name.as_bytes();
    let hex_at = |i: usize| bytes.get(i).is_some_and(|b| (*b as char).is_ascii_hexdigit());
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    'scan: while i < bytes.len() {
        if bytes[i] == b'_' {
            // 先试 2 位定长(`_HH_`),不匹配再试 4 位(`_HHHH_`)。
            for (width, span) in [(2usize, 4usize), (4usize, 6usize)] {
                if i + span <= bytes.len()
                    && bytes[i + span - 1] == b'_'
                    && (1..=width).all(|k| hex_at(i + k))
                {
                    let mut code = 0u32;
                    for k in 1..=width {
                        code = code * 16 + (bytes[i + k] as char).to_digit(16).unwrap();
                    }
                    if let Some(decoded) = char::from_u32(code) {
                        out.push(decoded);
                        i += span;
                        continue 'scan;
                    }
                }
            }
        }
        let ch = name[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compliant_names_pass_through() {
        // 不含 `_` 的合法名直通(零开销);含 `_` 名必须全量转义后仍可逆。
        for name in ["web-search", "getWeather", "a"] {
            assert_eq!(sanitize(name, Profile::Anthropic), name);
            assert_eq!(desanitize(name), name);
        }
        for name in ["get_weather", "mcp__server__tool"] {
            let encoded = sanitize(name, Profile::Anthropic);
            assert!(encoded.contains("_5F_"), "{name} → {encoded}");
            assert_eq!(desanitize(&encoded), name);
        }
    }

    #[test]
    fn illegal_chars_roundtrip() {
        for name in ["server.tool", "mcp:a:b", "中文工具", "a b/c"] {
            let encoded = sanitize(name, Profile::Anthropic);
            assert!(
                encoded.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{name} → {encoded}"
            );
            assert_eq!(desanitize(&encoded), name, "{name} → {encoded}");
        }
    }

    #[test]
    fn literal_underscores_are_disambiguated() {
        // 原名内含 `_HH_` 形态:必须先行转义,否则与编码歧义。
        let name = "tool_AB_x";
        let encoded = sanitize(name, Profile::Anthropic);
        assert_eq!(encoded, "tool_5F_AB_5F_x", "{encoded}");
        assert_eq!(desanitize(&encoded), name);
        // 单个 `_` + 恰好两位十六进制 + `_` 的原名同样安全。
        let name = "x_2E_y";
        let encoded = sanitize(name, Profile::Anthropic);
        assert_eq!(desanitize(&encoded), name);
    }

    #[test]
    fn gemini_profile_guards_first_char_and_length() {
        assert_eq!(sanitize("2cool", Profile::Gemini), "_2cool");
        assert_eq!(desanitize("_2cool"), "_2cool");
        let long = format!("{}.{}", "a".repeat(40), "b".repeat(40));
        let encoded = sanitize(&long, Profile::Gemini);
        assert!(encoded.len() <= 63);
    }

    #[test]
    fn third_party_names_without_our_escaping_pass_decode() {
        // 上游自造的普通名不被误解码。
        assert_eq!(desanitize("search_web"), "search_web");
        assert_eq!(desanitize("tool_2Ex"), "tool_2Ex", "缺右界定符不解码");
    }
}
