//! 农历（公历 → 农历）换算，用于锁屏日期“十月初九 · 星期五”。
//!
//! 数据表覆盖农历 1900–2100 年（公历 1900-01-31 至 2101-01-28），每年一个数：
//! - 低 4 位：闰月月份（0 表示无闰月）；
//! - 第 4–15 位：正月到十二月的大小（第 15 位为正月，……第 4 位为十二月；1 = 大月 30 天，0 = 小月 29 天）；
//! - 第 16 位：闰月的大小（1 = 30 天，0 = 29 天）。
//!
//! 数据来自紫金山天文台历表（与常见的 lunarInfo 表相同），基准为公历 1900-01-31 = 农历 1900 年正月初一。

/// 农历 1900–2100 年数据（每行十年）。
#[rustfmt::skip]
const LUNAR_INFO: [u32; 201] = [
    0x04bd8, 0x04ae0, 0x0a570, 0x054d5, 0x0d260, 0x0d950, 0x16554, 0x056a0, 0x09ad0, 0x055d2, // 1900
    0x04ae0, 0x0a5b6, 0x0a4d0, 0x0d250, 0x1d255, 0x0b540, 0x0d6a0, 0x0ada2, 0x095b0, 0x14977, // 1910
    0x04970, 0x0a4b0, 0x0b4b5, 0x06a50, 0x06d40, 0x1ab54, 0x02b60, 0x09570, 0x052f2, 0x04970, // 1920
    0x06566, 0x0d4a0, 0x0ea50, 0x16a95, 0x05ad0, 0x02b60, 0x186e3, 0x092e0, 0x1c8d7, 0x0c950, // 1930
    0x0d4a0, 0x1d8a6, 0x0b550, 0x056a0, 0x1a5b4, 0x025d0, 0x092d0, 0x0d2b2, 0x0a950, 0x0b557, // 1940
    0x06ca0, 0x0b550, 0x15355, 0x04da0, 0x0a5b0, 0x14573, 0x052b0, 0x0a9a8, 0x0e950, 0x06aa0, // 1950
    0x0aea6, 0x0ab50, 0x04b60, 0x0aae4, 0x0a570, 0x05260, 0x0f263, 0x0d950, 0x05b57, 0x056a0, // 1960
    0x096d0, 0x04dd5, 0x04ad0, 0x0a4d0, 0x0d4d4, 0x0d250, 0x0d558, 0x0b540, 0x0b6a0, 0x195a6, // 1970
    0x095b0, 0x049b0, 0x0a974, 0x0a4b0, 0x0b27a, 0x06a50, 0x06d40, 0x0af46, 0x0ab60, 0x09570, // 1980
    0x04af5, 0x04970, 0x064b0, 0x074a3, 0x0ea50, 0x06b58, 0x05ac0, 0x0ab60, 0x096d5, 0x092e0, // 1990
    0x0c960, 0x0d954, 0x0d4a0, 0x0da50, 0x07552, 0x056a0, 0x0abb7, 0x025d0, 0x092d0, 0x0cab5, // 2000
    0x0a950, 0x0b4a0, 0x0baa4, 0x0ad50, 0x055d9, 0x04ba0, 0x0a5b0, 0x15176, 0x052b0, 0x0a930, // 2010
    0x07954, 0x06aa0, 0x0ad50, 0x05b52, 0x04b60, 0x0a6e6, 0x0a4e0, 0x0d260, 0x0ea65, 0x0d530, // 2020
    0x05aa0, 0x076a3, 0x096d0, 0x04afb, 0x04ad0, 0x0a4d0, 0x1d0b6, 0x0d250, 0x0d520, 0x0dd45, // 2030
    0x0b5a0, 0x056d0, 0x055b2, 0x049b0, 0x0a577, 0x0a4b0, 0x0aa50, 0x1b255, 0x06d20, 0x0ada0, // 2040
    0x14b63, 0x09370, 0x049f8, 0x04970, 0x064b0, 0x168a6, 0x0ea50, 0x06b20, 0x1a6c4, 0x0aae0, // 2050
    0x092e0, 0x0d2e3, 0x0c960, 0x0d557, 0x0d4a0, 0x0da50, 0x05d55, 0x056a0, 0x0a6d0, 0x055d4, // 2060
    0x052d0, 0x0a9b8, 0x0a950, 0x0b4a0, 0x0b6a6, 0x0ad50, 0x055a0, 0x0aba4, 0x0a5b0, 0x052b0, // 2070
    0x0b273, 0x06930, 0x07337, 0x06aa0, 0x0ad50, 0x14b55, 0x04b60, 0x0a570, 0x054e4, 0x0d160, // 2080
    0x0e968, 0x0d520, 0x0daa0, 0x16aa6, 0x056d0, 0x04ae0, 0x0a9d4, 0x0a2d0, 0x0d150, 0x0f252, // 2090
    0x0d520, // 2100
];

/// 数据表的第一年。
const FIRST_YEAR: i32 = 1900;

/// 农历日期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LunarDate {
    /// 农历年（以正月初一为界）。
    pub year: i32,
    /// 月（1–12）。
    pub month: u32,
    /// 日（1–30）。
    pub day: u32,
    /// 是否闰月。
    pub leap: bool,
}

/// 闰月月份（0 = 无）。
fn leap_month(info: u32) -> u32 {
    info & 0xf
}

/// 某月（非闰月）天数。
fn month_days(info: u32, month: u32) -> u32 {
    if info & (0x10000 >> month) != 0 {
        30
    } else {
        29
    }
}

/// 闰月天数（无闰月为 0）。
fn leap_days(info: u32) -> u32 {
    match leap_month(info) {
        0 => 0,
        _ if info & 0x10000 != 0 => 30,
        _ => 29,
    }
}

/// 农历年总天数。
fn year_days(info: u32) -> u32 {
    (1..=12).map(|m| month_days(info, m)).sum::<u32>() + leap_days(info)
}

/// 公历日期 → 自 1970-01-01 起的天数（proleptic Gregorian）。
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

impl LunarDate {
    /// 公历 → 农历。超出数据表范围（1900-01-31 之前、2101-01-28 及之后）或日期无效时返回 `None`。
    pub fn from_gregorian(year: i32, month: u32, day: u32) -> Option<LunarDate> {
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        let mut offset = days_from_civil(year, month, day) - days_from_civil(1900, 1, 31);
        if offset < 0 {
            return None;
        }
        for (i, &info) in LUNAR_INFO.iter().enumerate() {
            let len = year_days(info) as i64;
            if offset >= len {
                offset -= len;
                continue;
            }
            let leap = leap_month(info);
            for m in 1..=12 {
                let len = month_days(info, m) as i64;
                if offset < len {
                    return Some(LunarDate {
                        year: FIRST_YEAR + i as i32,
                        month: m,
                        day: offset as u32 + 1,
                        leap: false,
                    });
                }
                offset -= len;
                if m == leap {
                    let len = leap_days(info) as i64;
                    if offset < len {
                        return Some(LunarDate {
                            year: FIRST_YEAR + i as i32,
                            month: m,
                            day: offset as u32 + 1,
                            leap: true,
                        });
                    }
                    offset -= len;
                }
            }
        }
        None
    }

    /// 月份名，如“正月”“闰四月”“冬月”“腊月”。
    pub fn month_name(&self) -> String {
        const NAMES: [&str; 12] = [
            "正", "二", "三", "四", "五", "六", "七", "八", "九", "十", "冬", "腊",
        ];
        let leap = if self.leap { "闰" } else { "" };
        format!("{leap}{}月", NAMES[(self.month as usize - 1) % 12])
    }

    /// 日名，如“初九”“十五”“廿一”“三十”。
    pub fn day_name(&self) -> String {
        const DIGITS: [&str; 10] = ["十", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
        let d = self.day as usize;
        match d {
            1..=10 => format!("初{}", DIGITS[d % 10]),
            11..=19 => format!("十{}", DIGITS[d % 10]),
            20 => "二十".into(),
            21..=29 => format!("廿{}", DIGITS[d % 10]),
            _ => "三十".into(),
        }
    }

    /// 完整写法，如“十月初九”。
    pub fn name(&self) -> String {
        format!("{}{}", self.month_name(), self.day_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lunar(y: i32, m: u32, d: u32) -> (i32, u32, u32, bool) {
        let l = LunarDate::from_gregorian(y, m, d).expect("在数据表范围内");
        (l.year, l.month, l.day, l.leap)
    }

    #[test]
    fn spring_festivals() {
        // 春节（正月初一）的公历日期。
        for (y, m, d) in [
            (1900, 1, 31),
            (1990, 1, 27),
            (2000, 2, 5),
            (2008, 2, 7),
            (2010, 2, 14),
            (2012, 1, 23),
            (2015, 2, 19),
            (2016, 2, 8),
            (2017, 1, 28),
            (2018, 2, 16),
            (2019, 2, 5),
            (2020, 1, 25),
            (2021, 2, 12),
            (2022, 2, 1),
            (2023, 1, 22),
            (2024, 2, 10),
            (2025, 1, 29),
            (2026, 2, 17),
            (2027, 2, 6),
            (2028, 1, 26),
        ] {
            assert_eq!(lunar(y, m, d), (y, 1, 1, false), "{y}-{m}-{d}");
        }
        // 春节前一天是上一年的腊月最后一天（除夕）。
        let eve = LunarDate::from_gregorian(2024, 2, 9).unwrap();
        assert_eq!((eve.year, eve.month), (2023, 12));
        assert!(eve.day == 29 || eve.day == 30);
    }

    #[test]
    fn known_dates() {
        // 中秋节（八月十五）。
        assert_eq!(lunar(2023, 9, 29), (2023, 8, 15, false));
        assert_eq!(lunar(2024, 9, 17), (2024, 8, 15, false));
        assert_eq!(lunar(2025, 10, 6), (2025, 8, 15, false));
        assert_eq!(lunar(2026, 9, 25), (2026, 8, 15, false));
        // 端午节（五月初五）。
        assert_eq!(lunar(2024, 6, 10), (2024, 5, 5, false));
        // 开国大典：农历己丑年八月初十。
        assert_eq!(lunar(1949, 10, 1), (1949, 8, 10, false));
        // 北京奥运会开幕：农历戊子年七月初八。
        assert_eq!(lunar(2008, 8, 8), (2008, 7, 8, false));
        // 本仓库截图里的日期。
        assert_eq!(lunar(2026, 10, 30), (2026, 9, 21, false));
    }

    #[test]
    fn leap_months() {
        // 2020 年闰四月、2023 年闰二月、2025 年闰六月的初一。
        assert_eq!(lunar(2020, 5, 23), (2020, 4, 1, true));
        assert_eq!(lunar(2023, 3, 22), (2023, 2, 1, true));
        assert_eq!(lunar(2025, 7, 25), (2025, 6, 1, true));
        // 闰月前一天是同名的普通月最后一天。
        let before = LunarDate::from_gregorian(2023, 3, 21).unwrap();
        assert_eq!((before.month, before.leap), (2, false));
        assert_eq!(
            LunarDate::from_gregorian(2023, 3, 22).unwrap().month_name(),
            "闰二月"
        );
    }

    #[test]
    fn range_and_names() {
        assert_eq!(LunarDate::from_gregorian(1900, 1, 30), None);
        assert!(LunarDate::from_gregorian(2100, 12, 31).is_some());
        assert_eq!(LunarDate::from_gregorian(2101, 2, 1), None);
        assert_eq!(LunarDate::from_gregorian(2024, 13, 1), None);
        // 每年天数在 353–385 之间。
        for info in LUNAR_INFO {
            assert!((353..=385).contains(&year_days(info)), "{info:#x}");
        }
        let d = |m, day| LunarDate {
            year: 2026,
            month: m,
            day,
            leap: false,
        };
        assert_eq!(d(10, 9).name(), "十月初九");
        assert_eq!(d(1, 1).name(), "正月初一");
        assert_eq!(d(11, 10).name(), "冬月初十");
        assert_eq!(d(12, 30).name(), "腊月三十");
        assert_eq!(d(9, 21).name(), "九月廿一");
        assert_eq!(d(8, 15).name(), "八月十五");
        assert_eq!(d(8, 20).name(), "八月二十");
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
        );
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1
        );
    }
}
