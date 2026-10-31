//! 更新包签名：ed25519，签的是 `manifest.toml` 的原始字节。
//!
//! 密钥用标准的 PEM 格式，与 OpenSSL 互通：
//!
//! - 私钥：PKCS#8（`-----BEGIN PRIVATE KEY-----`），`openssl genpkey -algorithm ed25519` 生成的就是这种；
//! - 公钥：SubjectPublicKeyInfo（`-----BEGIN PUBLIC KEY-----`），`openssl pkey -pubout` 的输出。
//!
//! 所以也可以离线用 OpenSSL 签名：`openssl pkeyutl -sign -rawin -inkey key.pem -in manifest.toml`。
//! 设备上信任的公钥放在 `/usr/share/loong/update-keys/*.pub`（只读根分区，随系统更新）。

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::error::{IoContext, Result, err};

/// ed25519 签名的长度。
pub const SIGNATURE_LEN: usize = 64;

/// Ed25519 的算法 OID（1.3.101.112）的 DER 编码。
const OID_ED25519: [u8; 5] = [0x06, 0x03, 0x2b, 0x65, 0x70];
/// SubjectPublicKeyInfo 的固定前缀（后接 32 字节公钥）。
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
/// PKCS#8 v1 私钥的固定前缀（后接 32 字节种子）。
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    let mut padding = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if padding > 0 {
            return None;
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (padding <= 2).then_some(out)
}

/// 取出 PEM 中指定标签的 DER 数据。
fn pem_decode(text: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)? + begin.len();
    let stop = start + text[start..].find(&end)?;
    base64_decode(&text[start..stop])
}

fn pem_encode(der: &[u8], label: &str) -> String {
    let b64 = base64_encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).expect("base64 是 ASCII"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// 解析 PEM 公钥（SubjectPublicKeyInfo）。
pub fn parse_public_pem(text: &str) -> Result<VerifyingKey> {
    let der = pem_decode(text, "PUBLIC KEY").ok_or_else(|| err!(BadSignature, "不是 PEM 公钥"))?;
    let raw: [u8; 32] = der
        .strip_prefix(&SPKI_PREFIX[..])
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| err!(BadSignature, "不是 ed25519 公钥"))?;
    VerifyingKey::from_bytes(&raw).map_err(|e| err!(BadSignature, "公钥无效: {e}"))
}

/// 解析 PEM 私钥（PKCS#8；也接受带公钥的 v2 格式）。
pub fn parse_secret_pem(text: &str) -> Result<SigningKey> {
    let der = pem_decode(text, "PRIVATE KEY").ok_or_else(|| err!(BadSignature, "不是 PEM 私钥"))?;
    let seed: [u8; 32] = if let Some(seed) = der.strip_prefix(&PKCS8_PREFIX[..]) {
        seed.try_into().ok()
    } else {
        // OneAsymmetricKey（RFC 8410 v2）：算法 OID 之后是 OCTET STRING { OCTET STRING(32) }
        der.windows(OID_ED25519.len())
            .position(|w| w == OID_ED25519)
            .and_then(|i| der.get(i + OID_ED25519.len()..))
            .and_then(|rest| rest.strip_prefix(&[0x04, 0x22, 0x04, 0x20][..]))
            .and_then(|rest| rest.get(..32))
            .and_then(|seed| seed.try_into().ok())
    }
    .ok_or_else(|| err!(BadSignature, "不是 ed25519 私钥"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// 公钥 → PEM。
pub fn public_pem(key: &VerifyingKey) -> String {
    let mut der = SPKI_PREFIX.to_vec();
    der.extend_from_slice(key.as_bytes());
    pem_encode(&der, "PUBLIC KEY")
}

/// 私钥 → PEM（PKCS#8 v1，与 `openssl genpkey` 相同）。
pub fn secret_pem(key: &SigningKey) -> String {
    let mut der = PKCS8_PREFIX.to_vec();
    der.extend_from_slice(key.as_bytes());
    pem_encode(&der, "PRIVATE KEY")
}

/// 用 `/dev/urandom` 生成新的密钥对。
pub fn generate() -> Result<SigningKey> {
    let mut seed = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut seed))
        .ctx("读取 /dev/urandom")?;
    Ok(SigningKey::from_bytes(&seed))
}

/// 签名。
pub fn sign(key: &SigningKey, message: &[u8]) -> [u8; SIGNATURE_LEN] {
    key.sign(message).to_bytes()
}

/// 公钥的简短指纹（公钥前 8 字节的十六进制），用于日志。
pub fn fingerprint(key: &VerifyingKey) -> String {
    crate::manifest::hex(&key.as_bytes()[..8])
}

/// 信任的公钥集合。
#[derive(Debug, Default, Clone)]
pub struct Keyring {
    keys: Vec<(String, VerifyingKey)>,
}

impl Keyring {
    /// 空集合。
    pub fn new() -> Keyring {
        Keyring::default()
    }

    /// 加入一个公钥。
    pub fn add(&mut self, name: impl Into<String>, key: VerifyingKey) {
        self.keys.push((name.into(), key));
    }

    /// 读入一个 PEM 公钥文件。
    pub fn add_file(&mut self, path: &Path) -> Result<()> {
        let text = fs::read_to_string(path).ctx(path.display())?;
        let key = parse_public_pem(&text).map_err(|e| e.context(path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.add(name, key);
        Ok(())
    }

    /// 读入目录中所有 `*.pub`（按文件名排序）。目录不存在时得到空集合。
    pub fn load_dir(dir: &Path) -> Result<Keyring> {
        let mut ring = Keyring::new();
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ring),
            Err(e) => return Err(e).ctx(dir.display()),
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "pub"))
            .collect();
        paths.sort();
        for path in paths {
            ring.add_file(&path)?;
        }
        Ok(ring)
    }

    /// 是否没有公钥。
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// 公钥个数。
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// 验证签名，返回签名所用公钥的名字。
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<&str> {
        if self.keys.is_empty() {
            return Err(err!(BadSignature, "没有可信的更新公钥，无法验证更新包"));
        }
        let signature: [u8; SIGNATURE_LEN] = signature
            .try_into()
            .map_err(|_| err!(BadSignature, "签名长度应为 {SIGNATURE_LEN} 字节"))?;
        let signature = Signature::from_bytes(&signature);
        self.keys
            .iter()
            .find(|(_, key)| key.verify_strict(message, &signature).is_ok())
            .map(|(name, _)| name.as_str())
            .ok_or_else(|| {
                err!(
                    BadSignature,
                    "更新包的签名无效（不是由可信的密钥签署，或清单被改动过）"
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    // 用 `openssl genpkey -algorithm ed25519` 生成的私钥，以及 `openssl pkey -pubout` 导出的公钥。
    const OPENSSL_SECRET: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIGgAGJBae+2u3NjMqULVesB8km2PlBp6Xl7EC2bjI4oM
-----END PRIVATE KEY-----
";
    const OPENSSL_PUBLIC: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEApIHmTmFXYRC+RnJRrcGTbXEEfVOGmIX/PobE94qSTMM=
-----END PUBLIC KEY-----
";

    #[test]
    fn base64_roundtrip() {
        for len in 0..40 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert!(base64_decode("TW?u").is_none());
        assert!(base64_decode("TW=u").is_none());
    }

    #[test]
    fn rfc8032_test_vector() {
        // RFC 8032 7.1 TEST 1：空消息
        let seed = crate::manifest::parse_hex32(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        )
        .unwrap();
        let key = SigningKey::from_bytes(&seed);
        assert_eq!(
            crate::manifest::hex(key.verifying_key().as_bytes()),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        assert_eq!(
            crate::manifest::hex(&sign(&key, b"")),
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        );
    }

    #[test]
    fn pem_roundtrip_and_openssl_compat() {
        let key = parse_secret_pem(OPENSSL_SECRET).unwrap();
        let public = parse_public_pem(OPENSSL_PUBLIC).unwrap();
        assert_eq!(key.verifying_key(), public);
        // 我们写出的 PEM 与 OpenSSL 的逐字节相同
        assert_eq!(secret_pem(&key), OPENSSL_SECRET);
        assert_eq!(public_pem(&public), OPENSSL_PUBLIC);

        let fresh = generate().unwrap();
        let again = parse_secret_pem(&secret_pem(&fresh)).unwrap();
        assert_eq!(again.as_bytes(), fresh.as_bytes());
        assert_eq!(
            parse_public_pem(&public_pem(&fresh.verifying_key())).unwrap(),
            fresh.verifying_key()
        );
        assert!(parse_public_pem(OPENSSL_SECRET).is_err());
        assert!(parse_secret_pem(OPENSSL_PUBLIC).is_err());
    }

    #[test]
    fn keyring_verifies_and_rejects() {
        let good = generate().unwrap();
        let other = generate().unwrap();
        let mut ring = Keyring::new();
        assert_eq!(
            ring.verify(b"x", &[0; 64]).unwrap_err().kind(),
            ErrorKind::BadSignature
        );
        ring.add("other.pub", other.verifying_key());
        ring.add("good.pub", good.verifying_key());
        let msg = b"format = 1\n";
        let sig = sign(&good, msg);
        assert_eq!(ring.verify(msg, &sig).unwrap(), "good.pub");
        // 改动消息或签名中的任何一位都会失败
        assert_eq!(
            ring.verify(b"format = 2\n", &sig).unwrap_err().kind(),
            ErrorKind::BadSignature
        );
        let mut bad = sig;
        bad[10] ^= 1;
        assert!(ring.verify(msg, &bad).is_err());
        assert!(ring.verify(msg, &sig[..63]).is_err());
        // 不在信任集合里的密钥签的
        let stranger = generate().unwrap();
        assert!(ring.verify(msg, &sign(&stranger, msg)).is_err());
    }

    #[test]
    fn openssl_interop() {
        // 有 openssl 时：OpenSSL 签名，我们验证（离线签名流程）。
        let have = std::process::Command::new("openssl")
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !have {
            eprintln!("跳过：没有 openssl");
            return;
        }
        let dir = crate::testutil::TempDir::new("openssl");
        let key = dir.path().join("key.pem");
        let msg = dir.path().join("manifest.toml");
        let sig = dir.path().join("manifest.sig");
        fs::write(&key, OPENSSL_SECRET).unwrap();
        fs::write(&msg, "format = 1\nversion = \"9\"\n").unwrap();
        let status = std::process::Command::new("openssl")
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(&key)
            .arg("-in")
            .arg(&msg)
            .arg("-out")
            .arg(&sig)
            .status()
            .unwrap();
        assert!(status.success());
        let mut ring = Keyring::new();
        ring.add("k", parse_public_pem(OPENSSL_PUBLIC).unwrap());
        let message = fs::read(&msg).unwrap();
        let signature = fs::read(&sig).unwrap();
        ring.verify(&message, &signature).unwrap();
        // 与我们自己的签名一致（ed25519 是确定性的）
        assert_eq!(
            signature,
            sign(&parse_secret_pem(OPENSSL_SECRET).unwrap(), &message)
        );
    }
}
