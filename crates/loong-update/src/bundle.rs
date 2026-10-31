//! 更新包（`update-<版本>-<机型>.loongup`）：一个 ustar 归档，成员依次是
//!
//! 1. `manifest.toml`：清单（[`Manifest`]），最大 64 KiB；
//! 2. `manifest.sig`：对清单原始字节的 ed25519 签名（64 字节）；
//! 3. 各个载荷，顺序、文件名、大小与清单中的 `[[payload]]` 一致。
//!
//! 读取时先验签、再解析清单，之后一边读载荷一边计算 SHA-256，读完一个载荷立即与清单比对。
//! 整个过程是流式的：数据只经过一个 1 MiB 的缓冲区，不会整个读进内存，
//! 来源可以是文件、块设备（例如 QEMU 里额外挂的一块盘）、管道或下载进程的输出。

use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use crate::{
    error::{Error, IoContext, Result, cancelled_io, err},
    manifest::{
        BootBackend, MANIFEST_NAME, MAX_MANIFEST_SIZE, Manifest, Payload, PayloadKind,
        SIGNATURE_NAME, Slot, hex,
    },
    sign::{self, Keyring, SIGNATURE_LEN},
    tar,
};

/// 读写缓冲区大小。
pub const CHUNK: usize = 1 << 20;

/// 更新包的来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// 本地文件或块设备：可以读两遍（先完整校验，再写入）。
    Path(PathBuf),
    /// `http://` / `https://`：用 curl 或 busybox wget 下载，边下载边写入。
    Url(String),
    /// 标准输入（`-`）。
    Stdin,
}

impl Source {
    /// 由命令行参数解析。
    pub fn parse(s: &str) -> Source {
        if s == "-" {
            Source::Stdin
        } else if s.starts_with("http://") || s.starts_with("https://") {
            Source::Url(s.to_owned())
        } else {
            Source::Path(PathBuf::from(s))
        }
    }

    /// 能否读两遍（先校验后写入）。
    pub fn is_seekable(&self) -> bool {
        matches!(self, Source::Path(_))
    }

    /// 打开。
    pub fn open(&self) -> Result<Input> {
        match self {
            Source::Path(p) => Ok(Input::File(File::open(p).ctx(p.display())?)),
            Source::Stdin => Ok(Input::Stdin(io::stdin())),
            Source::Url(url) => {
                let mut child = download_command(url)?
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .spawn()
                    .ctx("启动下载程序")?;
                let stdout = child.stdout.take().expect("已设置 piped");
                Ok(Input::Download(child, stdout))
            }
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Path(p) => write!(f, "{}", p.display()),
            Source::Url(u) => f.write_str(u),
            Source::Stdin => f.write_str("标准输入"),
        }
    }
}

fn have_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

fn download_command(url: &str) -> Result<Command> {
    // 完整性由签名保证，传输层不需要再做什么；HTTPS 需要 curl 或带 TLS 的 wget（ssl_client）。
    if have_program("curl") {
        let mut c = Command::new("curl");
        c.args(["-fsSL", "--retry", "3", "--", url]);
        Ok(c)
    } else if have_program("wget") {
        let mut c = Command::new("wget");
        c.args(["-q", "-O", "-", url]);
        Ok(c)
    } else {
        Err(err!(NotSupported, "没有 curl 或 wget，无法下载 {url}"))
    }
}

/// 打开的来源。
pub enum Input {
    /// 文件或块设备。
    File(File),
    /// 标准输入。
    Stdin(io::Stdin),
    /// 下载进程与它的标准输出。
    Download(Child, std::process::ChildStdout),
}

impl Read for Input {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Input::File(f) => f.read(buf),
            Input::Stdin(s) => s.read(buf),
            Input::Download(_, out) => out.read(buf),
        }
    }
}

impl Input {
    /// 读完后调用：下载失败（例如 HTTP 404）时返回错误。
    pub fn finish(self) -> Result<()> {
        if let Input::Download(mut child, out) = self {
            drop(out);
            let status = child.wait().ctx("等待下载程序")?;
            // 我们读到归档结尾就停了，下载程序可能因为管道关闭而被 SIGPIPE 结束，这不算错。
            if !status.success() && status.code().is_some() {
                return Err(err!(Io, "下载失败（{status}）"));
            }
        }
        Ok(())
    }
}

/// 进度回调与取消。
pub trait Progress {
    /// 又处理了 `bytes` 字节。
    fn advance(&self, bytes: u64);
    /// 是否已被取消（每处理一块数据检查一次）。
    fn cancelled(&self) -> bool {
        false
    }
}

/// 什么也不做的进度回调。
pub struct NoProgress;

impl Progress for NoProgress {
    fn advance(&self, _bytes: u64) {}
}

fn to_bundle_error(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            err!(InvalidBundle, "不是有效的更新包: {e}")
        }
        _ => Error::from(e),
    }
}

fn read_member(r: &mut impl Read, expected: &str, max: u64) -> Result<Vec<u8>> {
    let header = tar::read_header(r)
        .map_err(to_bundle_error)?
        .ok_or_else(|| err!(InvalidBundle, "不是有效的更新包：缺少 {expected}"))?;
    if header.name != expected {
        return Err(err!(
            InvalidBundle,
            "不是有效的更新包：期望 {expected}，实际是 {}",
            header.name
        ));
    }
    if header.size > max {
        return Err(err!(
            InvalidBundle,
            "{expected} 太大（{} 字节）",
            header.size
        ));
    }
    let mut data = vec![0u8; header.size as usize];
    r.read_exact(&mut data).map_err(to_bundle_error)?;
    tar::skip(r, tar::padding(header.size)).map_err(to_bundle_error)?;
    Ok(data)
}

/// 已验签的更新包读取器。
pub struct BundleReader<R: Read> {
    r: R,
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    signer: String,
}

/// 读载荷时交给回调的数据流：读出的数据同时进入 SHA-256。
pub struct PayloadStream<'a> {
    inner: io::Take<&'a mut dyn Read>,
    hasher: Sha256,
    progress: &'a dyn Progress,
}

impl Read for PayloadStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.progress.cancelled() {
            return Err(cancelled_io());
        }
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.progress.advance(n as u64);
        Ok(n)
    }
}

impl PayloadStream<'_> {
    /// 还没读的字节数。
    pub fn remaining(&self) -> u64 {
        self.inner.limit()
    }
}

impl<R: Read> BundleReader<R> {
    /// 读入清单与签名并验证。签名不对时**不解析**清单（不信任的数据不进入 TOML 解析器）。
    pub fn open(mut r: R, keyring: &Keyring) -> Result<BundleReader<R>> {
        let manifest_bytes = read_member(&mut r, MANIFEST_NAME, MAX_MANIFEST_SIZE)?;
        let signature = read_member(&mut r, SIGNATURE_NAME, SIGNATURE_LEN as u64)?;
        let signer = keyring.verify(&manifest_bytes, &signature)?.to_owned();
        let manifest = Manifest::parse(&manifest_bytes)?;
        Ok(BundleReader {
            r,
            manifest,
            manifest_bytes,
            signer,
        })
    }

    /// 清单。
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// 清单的原始字节（用来确认两遍读到的是同一个包）。
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// 签名所用公钥的名字。
    pub fn signer(&self) -> &str {
        &self.signer
    }

    /// 依次读出所有载荷。每个载荷调用一次 `f`，`f` 可以只读一部分（剩下的会被读掉）；
    /// 读完后校验 SHA-256，不符则返回 [`crate::ErrorKind::HashMismatch`]，后面的载荷不再读取。
    pub fn read_payloads<F>(mut self, progress: &dyn Progress, mut f: F) -> Result<R>
    where
        F: FnMut(&Payload, &mut PayloadStream<'_>) -> Result<()>,
    {
        let payloads = self.manifest.payloads.clone();
        for payload in &payloads {
            let header = tar::read_header(&mut self.r)
                .map_err(to_bundle_error)?
                .ok_or_else(|| err!(InvalidBundle, "更新包不完整：缺少 {}", payload.file))?;
            if header.name != payload.file || header.size != payload.size {
                return Err(err!(
                    InvalidBundle,
                    "更新包内容与清单不符：期望 {}（{} 字节），实际是 {}（{} 字节）",
                    payload.file,
                    payload.size,
                    header.name,
                    header.size
                ));
            }
            let mut stream = PayloadStream {
                inner: (&mut self.r as &mut dyn Read).take(payload.size),
                hasher: Sha256::new(),
                progress,
            };
            let result = f(payload, &mut stream).and_then(|()| {
                io::copy(&mut stream, &mut io::sink())
                    .map(|_| ())
                    .map_err(Error::from)
            });
            if progress.cancelled() {
                return Err(err!(Cancelled, "已取消"));
            }
            result?;
            if stream.remaining() != 0 {
                return Err(err!(
                    InvalidBundle,
                    "更新包不完整：{} 提前结束",
                    payload.file
                ));
            }
            let digest: [u8; 32] = stream.hasher.finalize().into();
            if digest != payload.digest() {
                return Err(err!(
                    HashMismatch,
                    "载荷 {} 的 SHA-256 与清单不符（期望 {}，实际 {}）：更新包已损坏或被篡改",
                    payload.file,
                    payload.sha256,
                    hex(&digest)
                ));
            }
            tar::skip(&mut self.r, tar::padding(payload.size)).map_err(to_bundle_error)?;
        }
        // 清单之外不能有别的成员。块设备上归档之后的内容不读。
        if let Some(extra) = tar::read_header(&mut self.r).map_err(to_bundle_error)? {
            return Err(err!(
                InvalidBundle,
                "更新包里有清单之外的文件 {}",
                extra.name
            ));
        }
        Ok(self.r)
    }

    /// 读完并校验所有载荷，不写任何地方。
    pub fn verify_all(self, progress: &dyn Progress) -> Result<R> {
        self.read_payloads(progress, |_, _| Ok(()))
    }
}

/// 打包时的一个载荷文件。
#[derive(Debug, Clone)]
pub struct PackItem {
    /// 种类。
    pub kind: PayloadKind,
    /// 只用于哪个 slot。
    pub slot: Option<Slot>,
    /// 本地文件。
    pub path: PathBuf,
}

impl PackItem {
    /// 解析命令行写法 `种类[@slot]=路径`，例如 `rootfs=root.erofs`、`uki@a=loong-a.efi`。
    pub fn parse(spec: &str) -> Result<PackItem> {
        let (what, path) = spec
            .split_once('=')
            .ok_or_else(|| err!(Failed, "载荷 {spec:?} 应写成 种类[@slot]=路径"))?;
        let (kind, slot) = match what.split_once('@') {
            Some((kind, slot)) => (
                kind,
                Some(Slot::parse(slot).ok_or_else(|| err!(Failed, "未知的 slot {slot:?}"))?),
            ),
            None => (what, None),
        };
        let kind =
            PayloadKind::parse(kind).ok_or_else(|| err!(Failed, "未知的载荷种类 {kind:?}"))?;
        Ok(PackItem {
            kind,
            slot,
            path: PathBuf::from(path),
        })
    }

    /// 在更新包里的文件名。
    fn file_name(&self) -> String {
        match (self.kind, self.slot) {
            (PayloadKind::Rootfs, None) => "rootfs.img".into(),
            (PayloadKind::Rootfs, Some(s)) => format!("rootfs-{s}.img"),
            (PayloadKind::Uki, Some(s)) => format!("loong-{s}.efi"),
            (PayloadKind::Uki, None) => "loong.efi".into(),
            (PayloadKind::Boot, Some(s)) => format!("boot_{s}.img"),
            (PayloadKind::Boot, None) => "boot.img".into(),
        }
    }
}

/// 打包参数（清单中除载荷以外的字段）。
#[derive(Debug, Clone)]
pub struct PackInfo {
    /// 版本。
    pub version: String,
    /// 机型。
    pub profile: String,
    /// 架构。
    pub arch: String,
    /// 启动后端。
    pub boot_backend: BootBackend,
    /// 最低版本。
    pub min_version: Option<String>,
    /// 根文件系统类型。
    pub root_fs: Option<String>,
    /// 生成时间（Unix 秒；可复现构建用 `SOURCE_DATE_EPOCH`）。
    pub created: u64,
}

/// 计算文件的 SHA-256 和大小（流式）。
pub fn hash_file(path: &Path) -> Result<([u8; 32], u64)> {
    let mut f = File::open(path).ctx(path.display())?;
    hash_reader(&mut f, u64::MAX, &NoProgress).map_err(|e| e.context(path.display()))
}

/// 读最多 `limit` 字节，返回 SHA-256 与实际读到的字节数。
pub fn hash_reader(
    r: &mut impl Read,
    limit: u64,
    progress: &dyn Progress,
) -> Result<([u8; 32], u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut r = r.take(limit);
    let mut total = 0u64;
    loop {
        if progress.cancelled() {
            return Err(err!(Cancelled, "已取消"));
        }
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        hasher.update(&buf[..n]);
        total += n as u64;
        progress.advance(n as u64);
    }
    Ok((hasher.finalize().into(), total))
}

/// Unix 秒 → `YYYY-MM-DDTHH:MM:SSZ`。
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Howard Hinnant 的 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// 打包：计算各载荷的哈希，生成并签署清单，写出 tar 归档。返回清单。
pub fn pack(
    info: &PackInfo,
    items: &[PackItem],
    key: &SigningKey,
    out: &mut impl Write,
) -> Result<Manifest> {
    let mut payloads = Vec::new();
    for item in items {
        let (digest, size) = hash_file(&item.path)?;
        payloads.push(Payload {
            kind: item.kind,
            slot: item.slot,
            file: item.file_name(),
            size,
            sha256: hex(&digest),
        });
    }
    let manifest = Manifest {
        format: crate::manifest::FORMAT,
        version: info.version.clone(),
        profile: info.profile.clone(),
        arch: info.arch.clone(),
        boot_backend: info.boot_backend,
        min_version: info.min_version.clone(),
        root_fs: info.root_fs.clone(),
        created: Some(rfc3339(info.created)),
        payloads,
    };
    manifest.validate()?;
    let text = manifest.to_toml();
    let signature = sign::sign(key, text.as_bytes());

    let mut write_member = |name: &str, data: &[u8]| -> io::Result<()> {
        tar::write_header(out, name, data.len() as u64, info.created)?;
        out.write_all(data)?;
        tar::write_padding(out, data.len() as u64)
    };
    write_member(MANIFEST_NAME, text.as_bytes()).ctx("写出更新包")?;
    write_member(SIGNATURE_NAME, &signature).ctx("写出更新包")?;
    for (item, payload) in items.iter().zip(&manifest.payloads) {
        tar::write_header(out, &payload.file, payload.size, info.created).ctx("写出更新包")?;
        let mut f = File::open(&item.path).ctx(item.path.display())?;
        // 打包期间文件被改动时，宁可失败也不产出哈希不符的包。
        let mut tee = HashingWriter {
            inner: &mut *out,
            hasher: Sha256::new(),
        };
        let copied = io::copy(&mut (&mut f).take(payload.size), &mut tee).ctx("写出更新包")?;
        let digest: [u8; 32] = tee.hasher.finalize().into();
        if copied != payload.size || digest != payload.digest() {
            return Err(err!(Failed, "{} 在打包过程中被修改了", item.path.display()));
        }
        tar::write_padding(out, payload.size).ctx("写出更新包")?;
    }
    tar::write_end(out).ctx("写出更新包")?;
    out.flush().ctx("写出更新包")?;
    Ok(manifest)
}

struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: Sha256,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{error::ErrorKind, testutil::TempDir};

    /// 测试用更新包：随机内容的根文件系统和两个 UKI。
    pub(crate) struct Fixture {
        pub dir: TempDir,
        pub key: SigningKey,
        pub keyring: Keyring,
        pub items: Vec<PackItem>,
    }

    pub(crate) fn pseudo_random(len: usize, seed: u8) -> Vec<u8> {
        let mut x = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    impl Fixture {
        pub(crate) fn new(backend: BootBackend) -> Fixture {
            let dir = TempDir::new("bundle");
            let key = sign::generate().unwrap();
            let mut keyring = Keyring::new();
            keyring.add("test.pub", key.verifying_key());
            let boot = backend.boot_payload();
            let items = vec![
                PackItem {
                    kind: PayloadKind::Rootfs,
                    slot: None,
                    path: dir.write("in/root.img", pseudo_random(3 * CHUNK + 123, 1)),
                },
                PackItem {
                    kind: boot,
                    slot: Some(Slot::A),
                    path: dir.write("in/a.img", pseudo_random(70_000, 2)),
                },
                PackItem {
                    kind: boot,
                    slot: Some(Slot::B),
                    path: dir.write("in/b.img", pseudo_random(70_001, 3)),
                },
            ];
            Fixture {
                dir,
                key,
                keyring,
                items,
            }
        }

        pub(crate) fn info(&self, version: &str, backend: BootBackend) -> PackInfo {
            PackInfo {
                version: version.into(),
                profile: "test-device".into(),
                arch: "x86_64".into(),
                boot_backend: backend,
                min_version: None,
                root_fs: Some("erofs".into()),
                created: 1_793_000_000,
            }
        }

        pub(crate) fn pack(&self, info: &PackInfo, name: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            let mut f = File::create(&path).unwrap();
            pack(info, &self.items, &self.key, &mut f).unwrap();
            path
        }
    }

    #[test]
    fn pack_then_read_streams_every_payload() {
        let fx = Fixture::new(BootBackend::UefiSdboot);
        let path = fx.pack(&fx.info("1.2.3", BootBackend::UefiSdboot), "u.loongup");
        assert_eq!(std::fs::metadata(&path).unwrap().len() % 512, 0);

        let reader = BundleReader::open(File::open(&path).unwrap(), &fx.keyring).unwrap();
        assert_eq!(reader.signer(), "test.pub");
        let m = reader.manifest().clone();
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.created.as_deref(), Some("2026-10-26T07:33:20Z"));
        assert_eq!(
            m.payloads
                .iter()
                .map(|p| p.file.as_str())
                .collect::<Vec<_>>(),
            ["rootfs.img", "loong-a.efi", "loong-b.efi"]
        );
        let mut seen = Vec::new();
        reader
            .read_payloads(&NoProgress, |p, s| {
                let mut data = Vec::new();
                // 只读 rootfs 的一部分：剩下的由读取器读掉并计入哈希
                if p.kind == PayloadKind::Rootfs {
                    s.take(1000).read_to_end(&mut data).unwrap();
                } else {
                    s.read_to_end(&mut data).unwrap();
                }
                seen.push((p.file.clone(), data));
                Ok(())
            })
            .unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].1, pseudo_random(1000, 1));
        assert_eq!(seen[2].1, pseudo_random(70_001, 3));
    }

    fn open_err(path: &Path, keyring: &Keyring) -> Error {
        match BundleReader::open(File::open(path).unwrap(), keyring) {
            Ok(r) => r.verify_all(&NoProgress).map(|_| ()).unwrap_err(),
            Err(e) => e,
        }
    }

    fn flip_byte(path: &Path, offset: u64) {
        let mut data = std::fs::read(path).unwrap();
        data[offset as usize] ^= 0x40;
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn detects_bad_signature_and_tampering() {
        let fx = Fixture::new(BootBackend::UefiSdboot);
        let info = fx.info("1.2.3", BootBackend::UefiSdboot);

        // 不认识的签名者
        let path = fx.pack(&info, "a.loongup");
        let mut stranger = Keyring::new();
        stranger.add("x.pub", sign::generate().unwrap().verifying_key());
        assert_eq!(open_err(&path, &stranger).kind(), ErrorKind::BadSignature);
        assert_eq!(
            open_err(&path, &Keyring::new()).kind(),
            ErrorKind::BadSignature
        );

        // 改了清单（版本号的一个字符）→ 签名不符
        let text = std::fs::read(&path).unwrap();
        let at = text.windows(7).position(|w| w == b"\"1.2.3\"").unwrap() as u64;
        flip_byte(&path, at + 1);
        assert_eq!(open_err(&path, &fx.keyring).kind(), ErrorKind::BadSignature);

        // 改了签名
        let path = fx.pack(&info, "b.loongup");
        let manifest_len = tar::read_header(&mut File::open(&path).unwrap())
            .unwrap()
            .unwrap()
            .size;
        let sig_at = 512 + manifest_len + tar::padding(manifest_len) + 512;
        flip_byte(&path, sig_at + 3);
        assert_eq!(open_err(&path, &fx.keyring).kind(), ErrorKind::BadSignature);

        // 改了载荷 → 哈希不符
        let path = fx.pack(&info, "c.loongup");
        let size = std::fs::metadata(&path).unwrap().len();
        flip_byte(&path, size / 2);
        let e = open_err(&path, &fx.keyring);
        assert_eq!(e.kind(), ErrorKind::HashMismatch, "{e}");
        assert!(e.message().contains("rootfs.img"), "{e}");

        // 截断
        let path = fx.pack(&info, "d.loongup");
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(size - 1024 - 512 * 10).unwrap();
        assert_eq!(
            open_err(&path, &fx.keyring).kind(),
            ErrorKind::InvalidBundle
        );

        // 不是更新包
        let junk = fx.dir.write("junk", pseudo_random(4096, 9));
        assert_eq!(
            open_err(&junk, &fx.keyring).kind(),
            ErrorKind::InvalidBundle
        );
        let empty = fx.dir.write("empty", b"");
        assert_eq!(
            open_err(&empty, &fx.keyring).kind(),
            ErrorKind::InvalidBundle
        );
    }

    #[test]
    fn trailing_data_after_archive_is_ignored() {
        // 例如更新包放在块设备上：归档结尾之后还有别的数据
        let fx = Fixture::new(BootBackend::AndroidAbl);
        let path = fx.pack(&fx.info("2", BootBackend::AndroidAbl), "t.loongup");
        let mut data = std::fs::read(&path).unwrap();
        data.extend(pseudo_random(5000, 4));
        std::fs::write(&path, data).unwrap();
        let reader = BundleReader::open(File::open(&path).unwrap(), &fx.keyring).unwrap();
        reader.verify_all(&NoProgress).unwrap();
    }

    #[test]
    fn pack_item_spec() {
        let item = PackItem::parse("uki@b=/x/y.efi").unwrap();
        assert_eq!((item.kind, item.slot), (PayloadKind::Uki, Some(Slot::B)));
        assert_eq!(item.path, PathBuf::from("/x/y.efi"));
        assert_eq!(item.file_name(), "loong-b.efi");
        assert_eq!(
            PackItem::parse("rootfs=r").unwrap().file_name(),
            "rootfs.img"
        );
        assert!(PackItem::parse("rootfs").is_err());
        assert!(PackItem::parse("kernel=x").is_err());
        assert!(PackItem::parse("uki@c=x").is_err());
    }

    #[test]
    fn source_parsing_and_dates() {
        assert_eq!(Source::parse("-"), Source::Stdin);
        assert_eq!(
            Source::parse("https://example.org/u.loongup"),
            Source::Url("https://example.org/u.loongup".into())
        );
        assert_eq!(Source::parse("/dev/vdb"), Source::Path("/dev/vdb".into()));
        assert!(Source::parse("/dev/vdb").is_seekable());
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_793_000_000), "2026-10-26T07:33:20Z");
    }
}
