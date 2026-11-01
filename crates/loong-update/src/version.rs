//! 版本号：`数字[.数字…][-预发布][+构建信息]`，例如 `0.1.1`、`0.2.0-rc.1`、`0.1.0-dev`。
//!
//! 比较规则与 SemVer 一致，但数字段的个数不限（缺少的段按 0 算，`1.0` 等于 `1.0.0`）：
//! 先逐段比较数字；数字相同时，带预发布后缀的版本更旧（`0.2.0-rc.1` < `0.2.0`）；
//! 预发布后缀按点分段比较，纯数字段按数值、且小于非数字段。构建信息（`+` 之后）不参与比较。

use std::{cmp::Ordering, fmt};

/// 解析过的版本号。
#[derive(Debug, Clone)]
pub struct Version {
    text: String,
    nums: Vec<u64>,
    pre: Vec<String>,
}

impl Version {
    /// 解析版本号；格式不对时返回说明。
    pub fn parse(s: &str) -> Result<Version, String> {
        let bad = |why: &str| format!("版本号 {s:?} 无效：{why}");
        if s.is_empty() || s.len() > 64 {
            return Err(bad("长度应为 1–64"));
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
        {
            return Err(bad("只能包含字母、数字和 . - + _"));
        }
        let core = s.split('+').next().unwrap_or("");
        let (nums, pre) = match core.split_once('-') {
            Some((nums, pre)) => (nums, Some(pre)),
            None => (core, None),
        };
        let nums = nums
            .split('.')
            .map(|n| {
                if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                    Err(bad("数字段不能为空或含非数字"))
                } else {
                    n.parse::<u64>().map_err(|_| bad("数字太大"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let parts: Vec<String> = pre.split('.').map(str::to_owned).collect();
                if parts.iter().any(String::is_empty) {
                    return Err(bad("预发布后缀的段不能为空"));
                }
                parts
            }
        };
        Ok(Version {
            text: s.to_owned(),
            nums,
            pre,
        })
    }

    /// 原始文本。
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

fn cmp_pre_part(a: &str, b: &str) -> Ordering {
    let num = |s: &str| {
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.nums.len().max(other.nums.len());
        for i in 0..n {
            let a = self.nums.get(i).copied().unwrap_or(0);
            let b = other.nums.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    match cmp_pre_part(a, b) {
                        Ordering::Equal => {}
                        o => return o,
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn ordering() {
        assert!(v("0.1.1") > v("0.1.0"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0") == v("1.0.0"));
        assert!(v("0.2.0") > v("0.2.0-rc.1"));
        assert!(v("0.2.0-rc.2") > v("0.2.0-rc.1"));
        assert!(v("0.2.0-rc.10") > v("0.2.0-rc.9"));
        assert!(v("0.2.0-rc") > v("0.2.0-1"));
        assert!(v("0.2.0-rc.1.1") > v("0.2.0-rc.1"));
        assert!(v("0.1.0") > v("0.1.0-dev"));
        assert!(v("0.1.1-dev") > v("0.1.0"));
        assert!(v("1.0.0+build.5") == v("1.0.0+build.1"));
        assert!(v("2") > v("1.99.99"));
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "",
            "abc",
            "1..2",
            "1.2.",
            ".1",
            "1.2-",
            "1.2-rc..1",
            "1 2",
            "1.2\n",
            "99999999999999999999",
        ] {
            assert!(Version::parse(s).is_err(), "{s:?} 应该无效");
        }
        assert_eq!(v("0.1.0-dev").as_str(), "0.1.0-dev");
    }
}
