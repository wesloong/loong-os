//! 本地时间（状态栏与锁屏）。时区由系统 /etc/localtime 或 TZ 环境变量决定。

use crate::lunar::LunarDate;

const WEEKDAYS: [&str; 7] = ["日", "一", "二", "三", "四", "五", "六"];

/// 返回 (时间, 日期)，例如 ("9:41", "九月廿一 · 星期五")：日期为农历 + 星期。
pub fn now_strings() -> (String, String) {
    // SAFETY: time 与 localtime_r 只写入我们提供的局部变量。
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    format_tm(
        tm.tm_hour,
        tm.tm_min,
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_wday,
    )
}

fn format_tm(
    hour: i32,
    min: i32,
    year: i32,
    month: i32,
    day: i32,
    weekday: i32,
) -> (String, String) {
    let wd = WEEKDAYS[weekday.rem_euclid(7) as usize];
    // 农历超出数据表范围（1900–2100）时退回公历。
    let date = match LunarDate::from_gregorian(year, month as u32, day as u32) {
        Some(lunar) => lunar.name(),
        None => format!("{month}月{day}日"),
    };
    (format!("{hour}:{min:02}"), format!("{date} · 星期{wd}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_lunar_date_and_weekday() {
        assert_eq!(
            super::format_tm(9, 41, 2026, 10, 30, 5),
            ("9:41".into(), "九月廿一 · 星期五".into())
        );
        assert_eq!(super::format_tm(23, 5, 2026, 1, 1, 4).0, "23:05");
        assert_eq!(super::format_tm(8, 0, 2024, 2, 10, 6).1, "正月初一 · 星期六");
        assert_eq!(super::format_tm(8, 0, 2200, 3, 1, 6).1, "3月1日 · 星期六");
    }
}
