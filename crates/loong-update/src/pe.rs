//! 读取 PE 文件（UKI）里的一个节，例如 `.osrel`（ukify 嵌入的 os-release）。
//!
//! systemd-boot 用 `.osrel` 里的 `IMAGE_VERSION`（没有时用 `VERSION_ID`）给 UKI 排序，
//! `loong-update status` 也从这里读出另一个 slot 的版本。

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use crate::error::{IoContext, Result, err};

/// 节内容的大小上限（`.osrel` 只有几百字节）。
const MAX_SECTION: u32 = 64 * 1024;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// 读出名为 `name` 的节；没有这个节时返回 `None`。
pub fn read_section(path: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let mut f = File::open(path).ctx(path.display())?;
    let bad = || err!(Failed, "{} 不是有效的 PE 文件", path.display());
    let mut head = vec![0u8; 4096];
    let n = read_up_to(&mut f, &mut head).ctx(path.display())?;
    head.truncate(n);
    if head.get(..2) != Some(b"MZ") {
        return Err(bad());
    }
    let pe = u32_at(&head, 0x3c).ok_or_else(bad)? as usize;
    if head.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(bad());
    }
    let count = usize::from(u16_at(&head, pe + 6).ok_or_else(bad)?);
    let opt_size = usize::from(u16_at(&head, pe + 20).ok_or_else(bad)?);
    let table = pe + 24 + opt_size;
    let table_len = count * 40;
    let headers = if table + table_len <= head.len() {
        head[table..table + table_len].to_vec()
    } else {
        let mut buf = vec![0u8; table_len];
        f.seek(SeekFrom::Start(table as u64)).ctx(path.display())?;
        f.read_exact(&mut buf).map_err(|_| bad())?;
        buf
    };
    for h in headers.chunks_exact(40) {
        let raw_name = &h[..8];
        let end = raw_name.iter().position(|&b| b == 0).unwrap_or(8);
        if &raw_name[..end] != name.as_bytes() {
            continue;
        }
        let vsize = u32_at(h, 8).ok_or_else(bad)?;
        let raw_size = u32_at(h, 16).ok_or_else(bad)?;
        let raw_ptr = u32_at(h, 20).ok_or_else(bad)?;
        // VirtualSize 是真实大小；SizeOfRawData 按文件对齐补齐过
        let size = if vsize == 0 {
            raw_size
        } else {
            vsize.min(raw_size)
        };
        if size > MAX_SECTION {
            return Err(err!(Failed, "{} 的 {name} 节太大", path.display()));
        }
        let mut data = vec![0u8; size as usize];
        f.seek(SeekFrom::Start(u64::from(raw_ptr)))
            .ctx(path.display())?;
        f.read_exact(&mut data).map_err(|_| bad())?;
        return Ok(Some(data));
    }
    Ok(None)
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// UKI 的版本：`.osrel` 里的 `IMAGE_VERSION`，没有时用 `VERSION_ID`（与 systemd-boot 一致）。
pub fn uki_version(path: &Path) -> Result<Option<String>> {
    let Some(osrel) = read_section(path, ".osrel")? else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&osrel);
    let env = crate::system::parse_env_file(text.trim_end_matches('\0'));
    Ok(env
        .get("IMAGE_VERSION")
        .or_else(|| env.get("VERSION_ID"))
        .cloned())
}

/// 测试用（本 crate 与 loong-sysd 的集成测试）：生成一个最小的 PE 文件，带给定的节。
#[doc(hidden)]
pub fn fake_pe(sections: &[(&str, &[u8])]) -> Vec<u8> {
    let pe = 0x80usize;
    let opt_size = 0xf0usize;
    let table = pe + 24 + opt_size;
    let mut data_at = (table + sections.len() * 40).div_ceil(512) * 512;
    let mut out = vec![0u8; data_at];
    out[..2].copy_from_slice(b"MZ");
    out[0x3c..0x40].copy_from_slice(&(pe as u32).to_le_bytes());
    out[pe..pe + 4].copy_from_slice(b"PE\0\0");
    out[pe + 6..pe + 8].copy_from_slice(&(sections.len() as u16).to_le_bytes());
    out[pe + 20..pe + 22].copy_from_slice(&(opt_size as u16).to_le_bytes());
    for (i, (name, content)) in sections.iter().enumerate() {
        let h = table + i * 40;
        out[h..h + name.len()].copy_from_slice(name.as_bytes());
        let raw = content.len().div_ceil(512) * 512;
        out[h + 8..h + 12].copy_from_slice(&(content.len() as u32).to_le_bytes());
        out[h + 16..h + 20].copy_from_slice(&(raw as u32).to_le_bytes());
        out[h + 20..h + 24].copy_from_slice(&(data_at as u32).to_le_bytes());
        out.extend_from_slice(content);
        out.resize(data_at + raw, 0);
        data_at += raw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn reads_osrel() {
        let t = TempDir::new("pe");
        let uki = t.write(
            "loong-b.efi",
            fake_pe(&[
                (".text", &[0x90; 700]),
                (
                    ".osrel",
                    b"NAME=\"Loong OS\"\nVERSION_ID=0.1.1\nIMAGE_VERSION=0.1.1\n",
                ),
                (".cmdline", b"quiet loong.slot=b"),
            ]),
        );
        assert_eq!(
            read_section(&uki, ".cmdline").unwrap().unwrap(),
            b"quiet loong.slot=b"
        );
        assert_eq!(uki_version(&uki).unwrap().as_deref(), Some("0.1.1"));
        assert_eq!(read_section(&uki, ".linux").unwrap(), None);

        let old = t.write("old.efi", fake_pe(&[(".osrel", b"VERSION_ID=0.0.9\n")]));
        assert_eq!(uki_version(&old).unwrap().as_deref(), Some("0.0.9"));
        let none = t.write("none.efi", fake_pe(&[(".text", b"x")]));
        assert_eq!(uki_version(&none).unwrap(), None);
        let junk = t.write("junk.efi", b"not a pe file");
        assert!(uki_version(&junk).is_err());
    }
}
