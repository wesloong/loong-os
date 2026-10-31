//! 最小的 ustar 读写：更新包就是一个普通的 tar 文件，可以用 `tar -tvf` 查看。
//!
//! 只支持普通文件成员；不支持 pax/GNU 扩展头（长文件名等用不到）。大于 8 GiB 的成员用
//! GNU 的 base-256 编码写大小（GNU tar、busybox tar 都能读）。

use std::io::{self, Read, Write};

/// tar 块大小。
pub const BLOCK: u64 = 512;

/// 成员头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// 文件名。
    pub name: String,
    /// 数据字节数。
    pub size: u64,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// 数据之后补齐到整块的字节数。
pub fn padding(size: u64) -> u64 {
    (BLOCK - size % BLOCK) % BLOCK
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(field).ok()?;
    let s = s.trim_matches(|c| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

fn parse_size(field: &[u8; 12]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        // GNU base-256：最高位是标志，其余按大端整数
        if field[0] != 0x80 || field[1..4].iter().any(|&b| b != 0) {
            return None;
        }
        Some(
            field[4..]
                .iter()
                .fold(0u64, |n, &b| (n << 8) | u64::from(b)),
        )
    } else {
        parse_octal(field)
    }
}

fn checksum(block: &[u8; 512]) -> u64 {
    block
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum()
}

/// 读下一个成员头。到达归档结尾（全零块）或数据结尾时返回 `None`。
pub fn read_header(r: &mut impl Read) -> io::Result<Option<Header>> {
    let mut block = [0u8; 512];
    let mut got = 0;
    while got < block.len() {
        match r.read(&mut block[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(bad("tar 头不完整")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    if block.iter().all(|&b| b == 0) {
        return Ok(None);
    }
    let stored = parse_octal(&block[148..156]).ok_or_else(|| bad("tar 头校验和格式不对"))?;
    if stored != checksum(&block) {
        return Err(bad("tar 头校验和不符"));
    }
    if &block[257..262] != b"ustar" {
        return Err(bad("不是 ustar 格式的 tar"));
    }
    match block[156] {
        b'0' | 0 => {}
        t => {
            return Err(bad(format!(
                "不支持的 tar 成员类型 {:?}（只支持普通文件）",
                t as char
            )));
        }
    }
    let field = |range: std::ops::Range<usize>| {
        let raw = &block[range];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        std::str::from_utf8(&raw[..end])
            .map(str::to_owned)
            .map_err(|_| bad("tar 文件名不是 UTF-8"))
    };
    let prefix = field(345..500)?;
    let mut name = field(0..100)?;
    if !prefix.is_empty() {
        name = format!("{prefix}/{name}");
    }
    let size_field: &[u8; 12] = block[124..136].try_into().expect("12 字节");
    let size = parse_size(size_field).ok_or_else(|| bad("tar 成员大小格式不对"))?;
    Ok(Some(Header { name, size }))
}

/// 写一个普通文件成员的头（文件名最长 100 字节）。
pub fn write_header(w: &mut impl Write, name: &str, size: u64, mtime: u64) -> io::Result<()> {
    if name.is_empty() || name.len() > 100 {
        return Err(bad(format!("tar 文件名 {name:?} 太长")));
    }
    let mut block = [0u8; 512];
    block[..name.len()].copy_from_slice(name.as_bytes());
    let octal = |block: &mut [u8; 512], at: usize, width: usize, value: u64| {
        let s = format!("{value:0w$o}", w = width - 1);
        block[at..at + width - 1].copy_from_slice(s.as_bytes());
    };
    octal(&mut block, 100, 8, 0o644); // mode
    octal(&mut block, 108, 8, 0); // uid
    octal(&mut block, 116, 8, 0); // gid
    if size < 0o777_7777_7777 {
        octal(&mut block, 124, 12, size);
    } else {
        block[124] = 0x80;
        block[128..136].copy_from_slice(&size.to_be_bytes());
    }
    octal(&mut block, 136, 12, mtime.min(0o777_7777_7777));
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[265..269].copy_from_slice(b"root");
    block[297..301].copy_from_slice(b"root");
    let sum = checksum(&block);
    let s = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(s.as_bytes());
    w.write_all(&block)
}

/// 写数据之后的补齐。
pub fn write_padding(w: &mut impl Write, size: u64) -> io::Result<()> {
    w.write_all(&[0u8; 512][..padding(size) as usize])
}

/// 写归档结尾（两个全零块）。
pub fn write_end(w: &mut impl Write) -> io::Result<()> {
    w.write_all(&[0u8; 1024])
}

/// 跳过（读掉）`n` 个字节。
pub fn skip(r: &mut impl Read, n: u64) -> io::Result<()> {
    let copied = io::copy(&mut r.take(n), &mut io::sink())?;
    if copied != n {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "tar 数据提前结束",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        write_header(&mut buf, "manifest.toml", 5, 1_700_000_000).unwrap();
        buf.extend_from_slice(b"hello");
        write_padding(&mut buf, 5).unwrap();
        write_header(&mut buf, "big.img", 9 << 30, 0).unwrap();
        write_end(&mut buf).unwrap();
        assert_eq!(buf.len() % 512, 0);

        let mut r = &buf[..];
        let h = read_header(&mut r).unwrap().unwrap();
        assert_eq!(
            h,
            Header {
                name: "manifest.toml".into(),
                size: 5
            }
        );
        skip(&mut r, 5 + padding(5)).unwrap();
        let h = read_header(&mut r).unwrap().unwrap();
        assert_eq!(h.size, 9 << 30, "大于 8 GiB 用 base-256");
        assert!(read_header(&mut r).unwrap().is_none());
        assert!(read_header(&mut &b""[..]).unwrap().is_none());
    }

    #[test]
    fn rejects_corruption() {
        let mut buf = Vec::new();
        write_header(&mut buf, "a", 1, 0).unwrap();
        let mut bad_sum = buf.clone();
        bad_sum[0] = b'b';
        assert!(read_header(&mut &bad_sum[..]).is_err());
        assert!(read_header(&mut &buf[..100]).is_err());
        let mut link = buf.clone();
        link[156] = b'2';
        // 改了类型也得重算校验和，否则先被校验和挡住
        let sum = checksum(link[..].try_into().unwrap());
        link[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let e = read_header(&mut &link[..]).unwrap_err();
        assert!(e.to_string().contains("类型"), "{e}");
        assert!(write_header(&mut Vec::new(), &"x".repeat(101), 0, 0).is_err());
    }

    #[test]
    fn readable_by_system_tar() {
        // 有 tar 命令时，确认它能列出我们写的归档。
        let Ok(out) = std::process::Command::new("tar").arg("--version").output() else {
            eprintln!("跳过：没有 tar");
            return;
        };
        if !out.status.success() {
            return;
        }
        let dir = crate::testutil::TempDir::new("tar");
        let path = dir.path().join("t.tar");
        let mut buf = Vec::new();
        for (name, data) in [("one.txt", &b"1"[..]), ("two.bin", &[7u8; 700][..])] {
            write_header(&mut buf, name, data.len() as u64, 0).unwrap();
            buf.extend_from_slice(data);
            write_padding(&mut buf, data.len() as u64).unwrap();
        }
        write_end(&mut buf).unwrap();
        std::fs::write(&path, &buf).unwrap();
        let out = std::process::Command::new("tar")
            .arg("-tf")
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "one.txt\ntwo.bin\n");
    }
}
