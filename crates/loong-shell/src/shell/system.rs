//! 系统状态：loong-sysd 报告的电量、Wi‑Fi、亮度在系统界面中的样子（纯数据，不涉及 D-Bus）。
//!
//! 数据由 `sysd_client` 在后台线程里从 `org.loong.System1` 读来，整份快照经事件循环交给
//! [`Shell::set_system_status`](super::Shell::set_system_status)。这里另有：
//!
//! - 状态栏符号的映射（[`StatusGlyphs`]）：只有映射后的结果变了才重画状态栏位图
//!   （电量 57.3% → 57.4% 不重画）；
//! - 控制中心发出的操作（[`SystemAction`]）与亮度请求的节流（[`Throttle`]）；
//! - 控制中心里的电量说明文字。
//!
//! loong-sysd 不在（没有配置总线、连不上、服务没运行）时，状态栏右侧**什么也不画**（只剩时间），
//! 控制中心的亮度、Wi‑Fi、电量显示“不可用”：宁可不显示，也不显示一个看起来像真的假电量。

use loong_ui::theme::Theme;

/// loong-sysd 是否可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Service {
    /// 没有配置总线、连不上总线，或者 `org.loong.System1` 没有运行。
    #[default]
    Absent,
    /// 正在运行（各项属性可能还没读到）。
    Running,
    /// 测试脚本给出的固定数据（无屏幕后端，保证截图可复现）。
    Demo,
}

/// 电池状态（`org.loong.System1.Power.State`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerState {
    Charging,
    Discharging,
    Full,
    /// 接着电源但没有充电（例如充电上限保护）。
    NotCharging,
    #[default]
    Unknown,
}

impl PowerState {
    pub fn parse(s: &str) -> Self {
        match s {
            "charging" => PowerState::Charging,
            "discharging" => PowerState::Discharging,
            "full" => PowerState::Full,
            "not-charging" => PowerState::NotCharging,
            _ => PowerState::Unknown,
        }
    }
}

/// 电源（`org.loong.System1.Power` 中界面用得到的属性）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PowerStatus {
    pub has_battery: bool,
    /// 电量 0.0–100.0。
    pub percentage: f64,
    pub state: PowerState,
    /// 预计剩余使用时间（秒），0 表示未知。
    pub time_to_empty: i64,
    /// 预计充满时间（秒），0 表示未知。
    pub time_to_full: i64,
    pub ac_online: bool,
}

/// Wi‑Fi 连接状态（`org.loong.System1.Wifi.State`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WifiState {
    #[default]
    Unavailable,
    Off,
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Roaming,
}

impl WifiState {
    pub fn parse(s: &str) -> Self {
        match s {
            "off" => WifiState::Off,
            "disconnected" => WifiState::Disconnected,
            "connecting" => WifiState::Connecting,
            "connected" => WifiState::Connected,
            "disconnecting" => WifiState::Disconnecting,
            "roaming" => WifiState::Roaming,
            _ => WifiState::Unavailable,
        }
    }

    /// 已经连上（漫游中也算）。
    pub fn is_connected(self) -> bool {
        matches!(self, WifiState::Connected | WifiState::Roaming)
    }
}

/// Wi‑Fi（`org.loong.System1.Wifi`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WifiStatus {
    /// iwd 在运行且有网卡。
    pub available: bool,
    pub enabled: bool,
    pub state: WifiState,
    pub ssid: String,
    /// 信号强度 0–100。
    pub strength: u32,
}

/// 背光（`org.loong.System1.Backlight`）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BacklightStatus {
    pub available: bool,
    /// 感知亮度 0.0–1.0（过渡中为目标值）。
    pub brightness: f64,
}

/// 一份完整的系统状态快照。各项为 `None` 表示还没读到（或者服务不在）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SystemStatus {
    pub service: Service,
    pub power: Option<PowerStatus>,
    pub wifi: Option<WifiStatus>,
    pub backlight: Option<BacklightStatus>,
}

impl SystemStatus {
    /// 服务不在：什么都不知道。
    pub fn absent() -> Self {
        SystemStatus::default()
    }

    /// 服务可用（含测试脚本的演示数据）。
    pub fn is_available(&self) -> bool {
        self.service != Service::Absent
    }

    /// 可以调节的亮度（服务可用、有背光设备）。
    pub fn brightness(&self) -> Option<f64> {
        self.backlight
            .filter(|b| self.is_available() && b.available)
            .map(|b| b.brightness.clamp(0.0, 1.0))
    }

    /// 可以开关的 Wi‑Fi（服务可用、iwd 在运行且有网卡）。
    pub fn wifi(&self) -> Option<&WifiStatus> {
        self.wifi
            .as_ref()
            .filter(|w| self.is_available() && w.available)
    }

    /// 电池（服务可用、有电池）。
    pub fn battery(&self) -> Option<&PowerStatus> {
        self.power
            .as_ref()
            .filter(|p| self.is_available() && p.has_battery)
    }
}

// ───────────────────────── 状态栏符号 ─────────────────────────

/// 低电量（含）以下电池用朱砂（设计规范 §3.5、§8.2；与 `ink::battery` 一致）。
pub const LOW_BATTERY_PERCENT: u8 = 20;

/// 状态栏右侧的电池符号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryGlyph {
    /// 整数百分比（显示的数字，也是填墨的长度）。
    pub percent: u8,
    /// 画藤黄闪电、石绿电量（充电中，或接着电源已充满）。
    pub charging: bool,
}

impl BatteryGlyph {
    /// 低电量：电量用朱砂（充电时不算）。
    pub fn is_low(self) -> bool {
        !self.charging && self.percent <= LOW_BATTERY_PERCENT
    }
}

/// 状态栏右侧画什么。只有它变了才重画状态栏位图（两种墨各一张）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusGlyphs {
    /// Wi‑Fi 的格数 1–3；`None` 不画（服务不在、关闭、没有连上）。
    pub wifi: Option<u8>,
    /// 电量与电池；`None` 不画（服务不在、没有电池）。
    pub battery: Option<BatteryGlyph>,
}

impl StatusGlyphs {
    pub fn from_status(status: &SystemStatus) -> Self {
        let wifi = status
            .wifi()
            .filter(|w| w.enabled && w.state.is_connected())
            .map(|w| wifi_bars(w.strength));
        let battery = status.battery().map(|p| BatteryGlyph {
            percent: p.percentage.clamp(0.0, 100.0).round() as u8,
            charging: p.state == PowerState::Charging
                || (p.state == PowerState::Full && p.ac_online),
        });
        StatusGlyphs { wifi, battery }
    }

    pub fn is_empty(&self) -> bool {
        self.wifi.is_none() && self.battery.is_none()
    }
}

/// 信号强度（0–100，iwd 的 −90 dBm 为 0、−30 dBm 为 100）→ 格数 1–3。已连上的网络至少一格。
pub fn wifi_bars(strength: u32) -> u8 {
    match strength {
        55.. => 3,
        25..=54 => 2,
        _ => 1,
    }
}

// ───────────────────────── 控制中心的操作 ─────────────────────────

/// 系统界面要求做的事（控制中心发出，`LoongState` 交给 loong-sysd 或写外观文件）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SystemAction {
    /// `Backlight.SetBrightness(value, animate = true)`。
    Brightness(f64),
    /// `Wifi.SetEnabled(enabled)`。
    WifiEnabled(bool),
    /// 写 `appearance.toml`（原子替换）；外观文件的监视随后切换整个系统界面。
    Theme(Theme),
}

/// 接收 [`SystemAction`] 的回调。
pub type ActionSink = Box<dyn FnMut(SystemAction)>;

/// 节流：连续变化的值（拖动亮度滑块）每隔至少 `interval` 秒才发一次，最后一个值一定发出。
///
/// [`Throttle::offer`] 时距上次发送够久就立即发；否则先记下，由每帧的 [`Throttle::poll`]
/// 补发；松手时 [`Throttle::flush`] 把记下的值立即发出。
#[derive(Debug, Clone)]
pub struct Throttle {
    interval: f64,
    last_sent: Option<f64>,
    pending: Option<f64>,
}

impl Throttle {
    pub fn new(interval: f64) -> Self {
        Throttle {
            interval,
            last_sent: None,
            pending: None,
        }
    }

    /// 新的值：可以立即发送时返回它。
    pub fn offer(&mut self, value: f64, now: f64) -> Option<f64> {
        if self
            .last_sent
            .is_none_or(|t| now - t >= self.interval - 1e-9)
        {
            self.pending = None;
            self.last_sent = Some(now);
            Some(value)
        } else {
            self.pending = Some(value);
            None
        }
    }

    /// 每帧调用：记下的值到时间了就返回它。
    pub fn poll(&mut self, now: f64) -> Option<f64> {
        let value = self.pending?;
        self.offer(value, now)
    }

    /// 立即发出记下的值（没有则为 `None`）。
    pub fn flush(&mut self, now: f64) -> Option<f64> {
        let value = self.pending.take()?;
        self.last_sent = Some(now);
        Some(value)
    }

    /// 没有待发的值。
    pub fn is_idle(&self) -> bool {
        self.pending.is_none()
    }
}

// ───────────────────────── 文字 ─────────────────────────

/// 时长（秒）→ “4 小时 45 分钟”“35 分钟”“2 小时”（按分钟四舍五入，至少 1 分钟）。
pub fn duration_text(secs: i64) -> String {
    let minutes = ((secs.max(0) as f64 / 60.0).round() as i64).max(1);
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, m) => format!("{m} 分钟"),
        (h, 0) => format!("{h} 小时"),
        (h, m) => format!("{h} 小时 {m} 分钟"),
    }
}

/// 控制中心电量模块的说明文字。
pub fn power_summary(status: &SystemStatus) -> String {
    if !status.is_available() {
        return "电量信息不可用".into();
    }
    let Some(p) = &status.power else {
        return "正在读取…".into();
    };
    if !p.has_battery {
        return if p.ac_online {
            "使用外接电源".into()
        } else {
            "没有电池".into()
        };
    }
    match p.state {
        PowerState::Charging if p.time_to_full > 0 => {
            format!("充电中 · 约 {}后充满", duration_text(p.time_to_full))
        }
        PowerState::Charging => "充电中".into(),
        PowerState::Full => "已充满".into(),
        PowerState::NotCharging => "已接通电源 · 未在充电".into(),
        PowerState::Discharging if p.time_to_empty > 0 => {
            format!("使用电池 · 约可用 {}", duration_text(p.time_to_empty))
        }
        PowerState::Discharging => "使用电池".into(),
        PowerState::Unknown => "电池".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> SystemStatus {
        SystemStatus {
            service: Service::Running,
            power: Some(PowerStatus {
                has_battery: true,
                percentage: 57.0,
                state: PowerState::Discharging,
                time_to_empty: 4 * 3600 + 45 * 60,
                time_to_full: 0,
                ac_online: false,
            }),
            wifi: Some(WifiStatus {
                available: true,
                enabled: true,
                state: WifiState::Connected,
                ssid: "听雨轩".into(),
                strength: 80,
            }),
            backlight: Some(BacklightStatus {
                available: true,
                brightness: 0.3,
            }),
        }
    }

    #[test]
    fn absent_service_draws_nothing() {
        assert!(StatusGlyphs::from_status(&SystemStatus::absent()).is_empty());
        // 断开后旧数据还在快照里也不画（服务不在就是不知道）。
        let stale = SystemStatus {
            service: Service::Absent,
            ..running()
        };
        assert!(StatusGlyphs::from_status(&stale).is_empty());
        assert_eq!(stale.brightness(), None);
        assert_eq!(power_summary(&stale), "电量信息不可用");
    }

    #[test]
    fn values_map_to_glyphs() {
        let s = running();
        assert_eq!(
            StatusGlyphs::from_status(&s),
            StatusGlyphs {
                wifi: Some(3),
                battery: Some(BatteryGlyph {
                    percent: 57,
                    charging: false
                }),
            }
        );
        // 小数变化不改变符号（不重画位图）。
        let mut t = s.clone();
        t.power.as_mut().unwrap().percentage = 57.4;
        assert_eq!(StatusGlyphs::from_status(&t), StatusGlyphs::from_status(&s));
        t.power.as_mut().unwrap().percentage = 57.6;
        assert_eq!(
            StatusGlyphs::from_status(&t).battery.unwrap().percent,
            58,
            "四舍五入"
        );
        // 充电：闪电；接着电源充满也画闪电；拔掉电源时的“已充满”不画。
        for (state, ac, bolt) in [
            (PowerState::Charging, true, true),
            (PowerState::Full, true, true),
            (PowerState::Full, false, false),
            (PowerState::NotCharging, true, false),
            (PowerState::Discharging, false, false),
        ] {
            let mut t = s.clone();
            let p = t.power.as_mut().unwrap();
            (p.state, p.ac_online) = (state, ac);
            assert_eq!(
                StatusGlyphs::from_status(&t).battery.unwrap().charging,
                bolt,
                "{state:?}"
            );
        }
        // 没有电池：不画电池与百分比。
        let mut t = s.clone();
        t.power.as_mut().unwrap().has_battery = false;
        assert_eq!(StatusGlyphs::from_status(&t).battery, None);
    }

    #[test]
    fn low_battery_is_twenty_percent_or_less_and_not_charging() {
        let glyph = |percent, charging| BatteryGlyph { percent, charging };
        assert!(glyph(20, false).is_low());
        assert!(glyph(3, false).is_low());
        assert!(!glyph(21, false).is_low());
        assert!(!glyph(10, true).is_low());
    }

    #[test]
    fn wifi_glyph_only_when_connected() {
        let mut s = running();
        for (strength, bars) in [(100, 3), (55, 3), (54, 2), (25, 2), (24, 1), (0, 1)] {
            s.wifi.as_mut().unwrap().strength = strength;
            assert_eq!(StatusGlyphs::from_status(&s).wifi, Some(bars), "{strength}");
        }
        for state in [
            WifiState::Disconnected,
            WifiState::Connecting,
            WifiState::Off,
        ] {
            s.wifi.as_mut().unwrap().state = state;
            assert_eq!(StatusGlyphs::from_status(&s).wifi, None, "{state:?}");
        }
        s.wifi.as_mut().unwrap().state = WifiState::Roaming;
        assert_eq!(StatusGlyphs::from_status(&s).wifi, Some(1));
        s.wifi.as_mut().unwrap().enabled = false;
        assert_eq!(StatusGlyphs::from_status(&s).wifi, None);
        // iwd 不在：控制中心里也不能开关。
        s.wifi.as_mut().unwrap().available = false;
        assert!(s.wifi().is_none());
    }

    #[test]
    fn throttle_limits_rate_and_keeps_the_last_value() {
        let mut t = Throttle::new(1.0 / 60.0);
        assert_eq!(t.offer(0.1, 0.0), Some(0.1));
        // 同一帧内的后续值先记下。
        assert_eq!(t.offer(0.2, 0.005), None);
        assert_eq!(t.offer(0.3, 0.010), None);
        assert!(!t.is_idle());
        assert_eq!(t.poll(0.012), None, "还没到时间");
        assert_eq!(t.poll(1.0 / 60.0), Some(0.3), "到时间补发最后一个值");
        assert!(t.is_idle());
        assert_eq!(t.poll(0.1), None);
        // 120Hz 的触摸：每秒最多 60 次。
        let mut t = Throttle::new(1.0 / 60.0);
        let sent = (0..120)
            .filter_map(|i| t.offer(i as f64, i as f64 / 120.0))
            .count();
        assert_eq!(sent, 60);
        // 松手：记下的值立即发出。
        assert_eq!(t.flush(1.0), Some(119.0));
        assert_eq!(t.flush(1.0), None);
    }

    #[test]
    fn durations_and_summaries() {
        assert_eq!(duration_text(30), "1 分钟");
        assert_eq!(duration_text(35 * 60), "35 分钟");
        assert_eq!(duration_text(2 * 3600), "2 小时");
        assert_eq!(duration_text(4 * 3600 + 45 * 60 + 10), "4 小时 45 分钟");
        let mut s = running();
        assert_eq!(power_summary(&s), "使用电池 · 约可用 4 小时 45 分钟");
        let p = s.power.as_mut().unwrap();
        (p.state, p.time_to_full, p.ac_online) = (PowerState::Charging, 40 * 60, true);
        assert_eq!(power_summary(&s), "充电中 · 约 40 分钟后充满");
        s.power.as_mut().unwrap().state = PowerState::Full;
        assert_eq!(power_summary(&s), "已充满");
        s.power = None;
        assert_eq!(power_summary(&s), "正在读取…");
    }
}
