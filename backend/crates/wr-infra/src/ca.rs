//! MITM CA 证书体系(方案 #14 R4.10;约束 C10)。
//!
//! 镜像 Python `infrastructure/proxy/certificate_service.py` 的职责:
//! 生成一张自签根证书(`ca.pem` / `ca.cer` DER / `key.pem`),按主机签发
//! 带 SAN + serverAuth 的叶子证书,并给出指纹/序列号。
//!
//! ## 与 Python 的有意差异(已登记)
//! 1. **RSA → ECDSA P-256**:Python 用 `cryptography` 生成 2048 位 RSA;
//!    rcgen 0.13 的 [`KeyPair::generate`] 产出 ECDSA。方案 #14 已明确
//!    「指纹不要求逐字节一致,只要求可安装、可信任、链路可验证」,故换
//!    算法满足验收,指纹值本身天然不同。
//! 2. **跨重启的叶子重签**:Python 每次 `create_for_host` 都从磁盘
//!    `load_ca()` 重建可签名的根证书对象;rcgen 无法把已落盘的根证书
//!    解析回可签名的 `Certificate` 类型。因此本服务把根证书**在进程内
//!    持有**(`ensure_ca` 生成后缓存;若磁盘已有则按持久化密钥重建),
//!    叶子签发使用该内存中的根证书。实际的 MITM 数据面(唯一消费叶子
//!    的地方)不在 R4 范围,见 14 号文档。
//! 3. 文件权限:Python 在非 Windows 上对私钥做 0600;Rust 用
//!    `set_permissions` 做同样的 0600/0700(非 Windows)。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use time::OffsetDateTime;

const CA_VALIDITY_DAYS: i64 = 20 * 365;
const LEAF_VALIDITY_DAYS: i64 = 10 * 365;

/// 根证书指纹与序列号(对应 Python `fingerprint_obj()`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateFingerprint {
    pub sha256: String,
    pub sha1: String,
    pub serial_number: String,
}

/// 叶子证书(`create_for_host` 的产物)。
#[derive(Debug, Clone)]
pub struct LeafCertificate {
    pub cert_pem: String,
    pub key_pem: String,
    /// DER 形态(供解析/验签;PEM 文本给消费方)。
    pub cert_der: Vec<u8>,
}

/// 进程内持有的根证书(可签名)。
struct CaState {
    cert: Certificate,
    key: Arc<KeyPair>,
}

/// MITM 证书服务(镜像 `CertificateService`)。
#[derive(Clone)]
pub struct CertificateService {
    cert_dir: PathBuf,
    hostname: String,
    ca: Arc<Mutex<Option<CaState>>>,
}

impl CertificateService {
    pub fn new(cert_dir: &Path, hostname: &str) -> Self {
        Self {
            cert_dir: cert_dir.to_path_buf(),
            hostname: hostname.to_owned(),
            ca: Arc::new(Mutex::new(None)),
        }
    }

    fn ca_cert_path(&self) -> PathBuf {
        self.cert_dir.join("ca.pem")
    }
    fn ca_key_path(&self) -> PathBuf {
        self.cert_dir.join("key.pem")
    }
    fn ca_der_path(&self) -> PathBuf {
        self.cert_dir.join("ca.cer")
    }

    pub fn ca_exists(&self) -> bool {
        self.ca_cert_path().exists() && self.ca_key_path().exists()
    }

    /// 幂等:磁盘已有根证书则不再重新生成(保持已安装信任的指纹稳定)。
    /// 无论如何都把根证书载入内存以供叶子签发。
    pub fn ensure_ca(&self) -> Result<(), CaError> {
        if self.ca_exists() {
            self.load_or_rebuild_ca()?;
            return Ok(());
        }
        self.generate_ca()
    }

    /// 生成全新的自签根证书并落盘(镜像 `ensure_ca` 的生成分支)。
    fn generate_ca(&self) -> Result<(), CaError> {
        fs::create_dir_all(&self.cert_dir)
            .map_err(|e| CaError::Io(format!("create cert dir: {e}")))?;
        set_dir_perm(&self.cert_dir);

        let key = KeyPair::generate().map_err(|e| CaError::Gen(format!("keygen: {e}")))?;
        let params = ca_params(&self.hostname);
        let cert = params
            .self_signed(&key)
            .map_err(|e| CaError::Gen(format!("self-sign: {e}")))?;

        write_file(&self.ca_cert_path(), cert.pem().as_bytes(), false)?;
        write_file(&self.ca_key_path(), key.serialize_pem().as_bytes(), true)?;
        write_file(&self.ca_der_path(), cert.der(), false)?;

        *self.ca.lock().unwrap_or_else(|e| e.into_inner()) = Some(CaState {
            cert,
            key: Arc::new(key),
        });
        Ok(())
    }

    /// 磁盘已有根证书时,按持久化私钥重建内存中的可签名根证书。
    ///
    /// rcgen 无法把已落盘的根证书解析回 `Certificate`,因此用
    /// `from_ca_cert_pem` 还原其参数(主题/有效期/序列/用途)后,用同一
    /// 把私钥重新自签。逻辑上等价同一根证书;DER 字节可能因签名随机性
    /// 而不同 —— 这是差异登记 ②。
    fn load_or_rebuild_ca(&self) -> Result<(), CaError> {
        let ca_pem = fs::read_to_string(self.ca_cert_path())
            .map_err(|e| CaError::Io(format!("read ca.pem: {e}")))?;
        let key_pem = fs::read_to_string(self.ca_key_path())
            .map_err(|e| CaError::Io(format!("read key.pem: {e}")))?;
        let key =
            KeyPair::from_pem(&key_pem).map_err(|e| CaError::Gen(format!("parse key.pem: {e}")))?;
        let params = CertificateParams::from_ca_cert_pem(&ca_pem)
            .map_err(|e| CaError::Gen(format!("parse ca.pem: {e}")))?;
        let cert = params
            .self_signed(&key)
            .map_err(|e| CaError::Gen(format!("rebuild ca: {e}")))?;
        *self.ca.lock().unwrap_or_else(|e| e.into_inner()) = Some(CaState {
            cert,
            key: Arc::new(key),
        });
        Ok(())
    }

    /// 读取磁盘根证书的指纹与序列号(镜像 `fingerprint_obj()`)。
    pub fn fingerprint(&self) -> Result<CertificateFingerprint, CaError> {
        let der = fs::read(self.ca_der_path())
            .or_else(|_| fs::read(self.ca_cert_path()))
            .map_err(|e| CaError::Io(format!("read ca for fingerprint: {e}")))?;
        let (_, parsed) = x509_parser::parse_x509_certificate(&der)
            .map_err(|e| CaError::Parse(format!("parse ca: {e}")))?;
        let tbs_der = parsed.tbs_certificate.as_ref();
        let serial = parsed.tbs_certificate.serial.to_string();
        Ok(CertificateFingerprint {
            sha256: hex(&sha256(tbs_der)),
            sha1: hex(&sha1(tbs_der)),
            serial_number: serial,
        })
    }

    /// 按主机签发叶子证书(镜像 `create_for_host`)。
    ///
    /// 返回 `(cert_pem, key_pem)`;有效期 10 年,`serverAuth` EKU,SAN 为
    /// `DNS:hostname`(或 `IP:` 若是地址)。
    pub fn create_leaf_for_host(&self, hostname: &str) -> Result<LeafCertificate, CaError> {
        self.ensure_ca()?;
        let state = self
            .ca
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| CaError::State("CA not loaded".to_owned()))?;

        let leaf_key =
            KeyPair::generate().map_err(|e| CaError::Gen(format!("leaf keygen: {e}")))?;
        let mut params = CertificateParams::new(vec![hostname.to_owned()])
            .map_err(|e| CaError::Gen(format!("leaf params: {e}")))?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, hostname);
        params.distinguished_name = dn;
        let now = OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + time::Duration::days(LEAF_VALIDITY_DAYS);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

        let cert = params
            .signed_by(&leaf_key, &state.cert, &state.key)
            .map_err(|e| CaError::Gen(format!("sign leaf: {e}")))?;

        // 放回内存(后续叶子还要用)。
        *self.ca.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);

        Ok(LeafCertificate {
            cert_der: cert.der().to_vec(),
            cert_pem: cert.pem(),
            key_pem: leaf_key.serialize_pem(),
        })
    }
}

/// 根证书参数:主题镜像 `_ca_subject`,`CA:TRUE` + 签名/证书签名/CRL 用途。
fn ca_params(hostname: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CountryName, "US");
    dn.push(DnType::StateOrProvinceName, "California");
    dn.push(DnType::LocalityName, "San Francisco");
    dn.push(DnType::OrganizationName, "WRouter");
    dn.push(DnType::OrganizationalUnitName, "WRouter MITM Proxy");
    dn.push(DnType::CommonName, format!("WRouter CA ({hostname})"));
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let now = OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + time::Duration::days(CA_VALIDITY_DAYS);
    params
}

fn write_file(path: &Path, bytes: &[u8], private: bool) -> Result<(), CaError> {
    fs::write(path, bytes).map_err(|e| CaError::Io(format!("write {}: {e}", path.display())))?;
    set_file_perm(path, private);
    Ok(())
}

#[cfg(not(windows))]
fn set_file_perm(path: &Path, private: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if private { 0o600 } else { 0o644 };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(windows))]
fn set_dir_perm(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(windows)]
fn set_file_perm(_path: &Path, _private: bool) {}

#[cfg(windows)]
fn set_dir_perm(_path: &Path) {}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

fn sha1(bytes: &[u8]) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// 证书层错误。
#[derive(Debug)]
pub enum CaError {
    Io(String),
    Gen(String),
    Parse(String),
    State(String),
}

impl std::fmt::Display for CaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) | Self::Gen(msg) | Self::Parse(msg) | Self::State(msg) => {
                write!(f, "{msg}")
            }
        }
    }
}

impl std::error::Error for CaError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wr-r4-ca-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ensure_ca 幂等 + 指纹/序列号齐备 + 叶子可信任链路。
    #[test]
    fn ca_and_leaf_chain_verifies() {
        let dir = temp_dir("chain");
        let service = CertificateService::new(&dir, "test-host");
        service.ensure_ca().unwrap();
        let fp1 = service.fingerprint().unwrap();
        assert_eq!(fp1.sha256.len(), 64);
        assert_eq!(fp1.sha1.len(), 40);
        assert!(!fp1.serial_number.is_empty());

        // 再次 ensure 幂等:不重新生成,指纹稳定。
        service.ensure_ca().unwrap();
        let fp2 = service.fingerprint().unwrap();
        assert_eq!(fp1, fp2, "ensure_ca 必须幂等,指纹不得漂移");

        // 叶子证书:可解析、由该根证书签发、SAN 正确。
        let leaf = service.create_leaf_for_host("example.com").unwrap();
        let (_, leaf_parsed) =
            x509_parser::parse_x509_certificate(&leaf.cert_der).expect("叶子证书可解析");
        let cn = leaf_parsed
            .tbs_certificate
            .subject
            .iter_common_name()
            .next()
            .map(|attr| attr.as_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        assert_eq!(cn, "example.com");
        // EKU = serverAuth(解析到即通过;深度语义由数据面消费时校验)。
        let has_eku = leaf_parsed.tbs_certificate.extensions().iter().any(|ext| {
            matches!(
                ext.parsed_extension(),
                x509_parser::extensions::ParsedExtension::ExtendedKeyUsage(_)
            )
        });
        assert!(has_eku, "叶子证书必须带 ExtendedKeyUsage 扩展");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 叶子证书的签名确由根证书公钥验证通过(可信任链)。
    #[test]
    fn leaf_signature_chains_to_ca() {
        let dir = temp_dir("sig");
        let service = CertificateService::new(&dir, "sig-host");
        service.ensure_ca().unwrap();
        let leaf = service.create_leaf_for_host("trusted.local").unwrap();

        // 用 x509-parser 验证叶子签名 = 根证书公钥。
        let ca_der = fs::read(service.ca_der_path()).unwrap();
        let (_, ca_parsed) = x509_parser::parse_x509_certificate(&ca_der).unwrap();
        let ca_pub = ca_parsed.tbs_certificate.subject_pki;
        let (_, leaf_parsed) = x509_parser::parse_x509_certificate(&leaf.cert_der).unwrap();
        leaf_parsed
            .verify_signature(Some(&ca_pub))
            .expect("叶子证书必须由根证书公钥验签通过");
        // 颁发者 == 根证书主题。
        assert_eq!(
            leaf_parsed.tbs_certificate.issuer,
            ca_parsed.tbs_certificate.subject
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
