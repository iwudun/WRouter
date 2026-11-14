//! 目标编码器共享的小工具（时间戳与短 id）。

pub(crate) fn created_now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub(crate) fn uuid_short() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn uuid_short_is_twelve_lowercase_hex_chars() {
        let id = super::uuid_short();
        assert_eq!(id.len(), 12);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
