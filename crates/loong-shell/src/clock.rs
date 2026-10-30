//! 本地时间（状态栏与锁屏）。时区由系统 /etc/localtime 或 TZ 环境变量决定。

const WEEKDAYS: [&str; 7] = ["日", "一", "二", "三", "四", "五", "六"];

/// 返回 (时间, 日期)，例如 ("9:41", "10月30日 星期五")。
pub fn now_strings() -> (String, String) {
    // SAFETY: time 与 localtime_r 只写入我们提供的局部变量。
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    format_tm(tm.tm_hour, tm.tm_min, tm.tm_mon + 1, tm.tm_mday, tm.tm_wday)
}

fn format_tm(hour: i32, min: i32, month: i32, day: i32, weekday: i32) -> (String, String) {
    let wd = WEEKDAYS[weekday.rem_euclid(7) as usize];
    (
        format!("{hour}:{min:02}"),
        format!("{month}月{day}日 星期{wd}"),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_like_ios() {
        assert_eq!(
            super::format_tm(9, 41, 10, 30, 5),
            ("9:41".into(), "10月30日 星期五".into())
        );
        assert_eq!(super::format_tm(23, 5, 1, 1, 0).0, "23:05");
    }
}
