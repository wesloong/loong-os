//! 写入目标与回读校验。
//!
//! - 分区（块设备；测试中是预先建好的普通文件）：大小固定，载荷必须放得下，不截断、不新建。
//! - 文件（ESP 上的 UKI）：新建或覆盖。
//!
//! 写完后 `fsync`，再用 `posix_fadvise(DONTNEED)` 丢掉页缓存，回读时就是从存储设备上读，
//! 能发现写入错误（坏块、存储控制器的问题）。

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use crate::{
    bundle::{CHUNK, Progress, hash_reader},
    error::{IoContext, Result, err},
};

/// 写入目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 固定大小的分区。
    Device(PathBuf),
    /// 普通文件（新建或覆盖）。
    File(PathBuf),
}

impl Target {
    /// 路径。
    pub fn path(&self) -> &Path {
        match self {
            Target::Device(p) | Target::File(p) => p,
        }
    }
}

/// 分区（或充当分区的文件）的容量。块设备通过 seek 到末尾得到。
pub fn device_size(path: &Path) -> Result<u64> {
    let mut f = File::open(path).ctx(path.display())?;
    f.seek(SeekFrom::End(0)).ctx(path.display())
}

/// 确认载荷放得下。
pub fn check_fits(target: &Target, size: u64) -> Result<()> {
    if let Target::Device(path) = target {
        let capacity = device_size(path)?;
        if size > capacity {
            return Err(err!(
                TooLarge,
                "{} 只有 {} MiB，放不下 {} MiB 的载荷",
                path.display(),
                capacity >> 20,
                size.div_ceil(1 << 20)
            ));
        }
    }
    Ok(())
}

fn drop_cache(f: &File, len: u64) {
    // 只是为了回读时绕过页缓存；不支持时（例如某些文件系统）忽略。
    let _ = rustix::fs::fadvise(f, 0, NonZeroU64::new(len), rustix::fs::Advice::DontNeed);
}

/// 把 `r` 中的 `size` 字节写入目标并落盘。
pub fn write(target: &Target, r: &mut impl Read, size: u64) -> Result<()> {
    let path = target.path();
    let mut f = match target {
        Target::Device(_) => File::options().write(true).open(path),
        Target::File(_) => File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path),
    }
    .ctx(path.display())?;
    let mut buf = vec![0u8; CHUNK];
    let mut left = size;
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = match r.read(&mut buf[..want]) {
            Ok(0) => return Err(err!(InvalidBundle, "载荷提前结束")),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        f.write_all(&buf[..n]).ctx(path.display())?;
        left -= n as u64;
    }
    f.sync_all().ctx(path.display())?;
    drop_cache(&f, size);
    Ok(())
}

/// 回读目标的前 `size` 字节，确认 SHA-256 与期望一致。
pub fn verify(
    target: &Target,
    size: u64,
    expected: &[u8; 32],
    progress: &dyn Progress,
) -> Result<()> {
    let path = target.path();
    let mut f = File::open(path).ctx(path.display())?;
    drop_cache(&f, size);
    let (digest, got) =
        hash_reader(&mut f, size, progress).map_err(|e| e.context(path.display()))?;
    if got != size || &digest != expected {
        return Err(err!(
            HashMismatch,
            "回读校验失败：{} 中的数据与写入的不一致（存储设备可能有问题）",
            path.display()
        ));
    }
    Ok(())
}

/// 把目录落盘（重命名之后调用，确保目录项写到存储上）。
pub fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .ctx(dir.display())
}

/// 原子地替换文件内容不是 FAT 能保证的；这里只是“先删后改名”的小工具。
pub fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).ctx(path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bundle::NoProgress, error::ErrorKind, testutil::TempDir};
    use sha2::{Digest, Sha256};

    #[test]
    fn device_write_and_verify() {
        let t = TempDir::new("storage");
        let dev = t.write("dev/part", vec![0xeeu8; 1 << 20]);
        let target = Target::Device(dev.clone());
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let digest: [u8; 32] = Sha256::digest(&data).into();

        check_fits(&target, data.len() as u64).unwrap();
        assert_eq!(
            check_fits(&target, (1 << 20) + 1).unwrap_err().kind(),
            ErrorKind::TooLarge
        );
        write(&target, &mut &data[..], data.len() as u64).unwrap();
        // 分区不截断：后面的旧数据还在
        assert_eq!(device_size(&dev).unwrap(), 1 << 20);
        verify(&target, data.len() as u64, &digest, &NoProgress).unwrap();
        let other: [u8; 32] = Sha256::digest(b"x").into();
        assert_eq!(
            verify(&target, data.len() as u64, &other, &NoProgress)
                .unwrap_err()
                .kind(),
            ErrorKind::HashMismatch
        );
        // 数据不够
        assert_eq!(
            write(&target, &mut &data[..10], 20).unwrap_err().kind(),
            ErrorKind::InvalidBundle
        );
        // 分区必须已存在
        assert!(
            write(
                &Target::Device(t.path().join("dev/none")),
                &mut &data[..],
                1
            )
            .is_err()
        );
    }

    #[test]
    fn file_write_truncates() {
        let t = TempDir::new("storage-file");
        let path = t.write("esp/x.efi", vec![1u8; 5000]);
        let target = Target::File(path.clone());
        write(&target, &mut &b"abc"[..], 3).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        remove_if_exists(&path).unwrap();
        remove_if_exists(&path).unwrap();
        sync_dir(t.path()).unwrap();
    }
}
