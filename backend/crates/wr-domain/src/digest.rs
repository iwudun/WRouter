//! 纯计算摘要工具(R4:幂等指纹与密钥派生)。
//!
//! 只做无 IO 的哈希/编码,放在 `wr-domain` 不违反纯度守护
//! (sha2 是纯计算库,R3 已为此放行 sha2/uuid)。

const HEX: &[u8; 16] = b"0123456789abcdef";

/// 小写十六进制编码(等价 Python `bytes.hex()`)。
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 小写十六进制(等价 Python `hashlib.sha256(x).hexdigest()`)。
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex_encode(&digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_python_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
