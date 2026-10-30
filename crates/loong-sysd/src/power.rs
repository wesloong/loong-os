//! 电池与电源：`/org/loong/System1/Power`。
//!
//! 数据来自 `/sys/class/power_supply/*`：
//!
//! - `type=Battery` 的条目是电池（`scope=Device` 的是触控笔、键盘等外设电池，忽略；
//!   `present=0` 的忽略）。电量优先用驱动给的 `capacity`，没有时用 `energy_now/energy_full`
//!   或 `charge_now/charge_full` 计算；多块电池按满电容量加权。
//! - `type=Mains/USB/Wireless` 的条目只看 `online`：任何一个在线就算接着外部电源。
//! - 剩余/充满时间优先用驱动给的 `time_to_empty_now`/`time_to_full_now`，否则用
//!   能量（µWh）÷ 功率（µW）或电荷（µAh）÷ 电流（µA）估算；功率做指数平滑，避免数字乱跳。
//!
//! 每 30 秒轮询一次；内核发出 `power_supply` 的 uevent 时（插拔充电器等）立即刷新。
//! 有变化时发 `PropertiesChanged`；放电中电量降到低电量/极低电量阈值时发 `LowBattery`。

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use zbus::{Connection, interface, object_server::SignalEmitter};

use crate::{
    bus::{self, Changed},
    sysfs::{self, Sysfs},
};

/// 对象路径。
pub const PATH: &str = "/org/loong/System1/Power";
/// 接口名。
pub const INTERFACE: &str = "org.loong.System1.Power";

/// 轮询间隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// 低电量阈值（%）。
pub const LOW_THRESHOLD: f64 = 15.0;
/// 极低电量阈值（%）。
pub const CRITICAL_THRESHOLD: f64 = 5.0;
/// 功率指数平滑系数（越小越平稳）。
const RATE_SMOOTHING: f64 = 0.3;
/// 超过这个时长的估算视为不可信（返回 0 = 未知）。
const MAX_ESTIMATE_SECS: f64 = 7.0 * 24.0 * 3600.0;

/// 电池状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// 未知。
    #[default]
    Unknown,
    /// 充电中。
    Charging,
    /// 放电中（使用电池）。
    Discharging,
    /// 已充满。
    Full,
    /// 接着电源但没有充电（例如充电上限保护）。
    NotCharging,
}

impl State {
    /// D-Bus 上的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            State::Unknown => "unknown",
            State::Charging => "charging",
            State::Discharging => "discharging",
            State::Full => "full",
            State::NotCharging => "not-charging",
        }
    }

    /// 解析 sysfs 的 `status`。
    fn parse(s: &str) -> State {
        match s.to_ascii_lowercase().as_str() {
            "charging" => State::Charging,
            "discharging" => State::Discharging,
            "full" => State::Full,
            "not charging" => State::NotCharging,
            _ => State::Unknown,
        }
    }
}

/// 低电量警告级别（有序：None < Low < Critical）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Warning {
    /// 正常。
    #[default]
    None,
    /// 低电量。
    Low,
    /// 极低电量。
    Critical,
}

impl Warning {
    /// D-Bus 上的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Warning::None => "none",
            Warning::Low => "low",
            Warning::Critical => "critical",
        }
    }
}

/// 一块电池的原始读数（单位同 sysfs：µWh、µW、µAh、µA、µV、秒）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Battery {
    /// `status`。
    pub status: State,
    /// `capacity`（%）。
    pub capacity: Option<f64>,
    /// `energy_now`。
    pub energy_now: Option<f64>,
    /// `energy_full`。
    pub energy_full: Option<f64>,
    /// `power_now`。
    pub power_now: Option<f64>,
    /// `charge_now`。
    pub charge_now: Option<f64>,
    /// `charge_full`。
    pub charge_full: Option<f64>,
    /// `current_now`（有的驱动放电时为负数）。
    pub current_now: Option<f64>,
    /// `voltage_now`。
    pub voltage_now: Option<f64>,
    /// `time_to_empty_now`。
    pub time_to_empty: Option<f64>,
    /// `time_to_full_now`。
    pub time_to_full: Option<f64>,
}

/// 估算剩余时间用的量纲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    /// µWh 与 µW。
    Energy,
    /// µAh 与 µA。
    Charge,
}

/// 现存量、满电量、速率（同一量纲；速率 0 表示这块电池空闲，`None` 表示不知道）。
#[derive(Debug, Clone, Copy)]
struct Reserve {
    now: f64,
    full: f64,
    rate: Option<f64>,
}

impl Battery {
    fn read(dir: &std::path::Path) -> Battery {
        let f = |file: &str| sysfs::read_f64(&dir.join(file));
        Battery {
            status: sysfs::read_string(&dir.join("status"))
                .map(|s| State::parse(&s))
                .unwrap_or_default(),
            capacity: f("capacity"),
            energy_now: f("energy_now"),
            energy_full: f("energy_full"),
            power_now: f("power_now"),
            charge_now: f("charge_now"),
            charge_full: f("charge_full"),
            current_now: f("current_now"),
            voltage_now: f("voltage_now"),
            time_to_empty: f("time_to_empty_now"),
            time_to_full: f("time_to_full_now"),
        }
    }

    fn reserve(&self, unit: Unit) -> Option<Reserve> {
        let (now, full, rate) = match unit {
            Unit::Energy => {
                let rate = self
                    .power_now
                    .map(f64::abs)
                    .or_else(|| Some((self.current_now? * self.voltage_now?).abs() / 1_000_000.0));
                (self.energy_now?, self.energy_full?, rate)
            }
            Unit::Charge => (
                self.charge_now?,
                self.charge_full?,
                self.current_now.map(f64::abs),
            ),
        };
        (full > 0.0 && now >= 0.0).then_some(Reserve {
            now: now.min(full),
            full,
            rate,
        })
    }

    /// 电量百分比：优先驱动给的 `capacity`。
    fn percentage(&self) -> Option<f64> {
        self.capacity
            .or_else(|| {
                let r = self
                    .reserve(Unit::Energy)
                    .or_else(|| self.reserve(Unit::Charge))?;
                Some(r.now / r.full * 100.0)
            })
            .map(|p| p.clamp(0.0, 100.0))
    }

    /// 加权用的满电容量。
    fn weight(&self) -> Option<f64> {
        self.energy_full.or(self.charge_full).filter(|w| *w > 0.0)
    }
}

/// 一次扫描的结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Supplies {
    /// 系统电池。
    pub batteries: Vec<Battery>,
    /// 外部电源是否在线；没有任何外部电源条目时为 `None`。
    pub ac_online: Option<bool>,
}

/// 扫描 `class/power_supply`。
pub fn read_supplies(sysfs: &Sysfs) -> Supplies {
    let mut out = Supplies::default();
    for (_, dir) in sysfs.class_entries("power_supply") {
        if sysfs::read_string(&dir.join("scope")).as_deref() == Some("Device") {
            continue;
        }
        match sysfs::read_string(&dir.join("type")).as_deref() {
            Some("Battery") => {
                if sysfs::read_num::<i64>(&dir.join("present")) == Some(0) {
                    continue;
                }
                out.batteries.push(Battery::read(&dir));
            }
            Some("Mains" | "USB" | "Wireless") => {
                if let Some(online) = sysfs::read_num::<i64>(&dir.join("online")) {
                    out.ac_online = Some(out.ac_online.unwrap_or(false) || online != 0);
                }
            }
            _ => {}
        }
    }
    out
}

/// 对外报告的电源状态。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    /// 是否有电池。
    pub has_battery: bool,
    /// 电量 0–100（保留一位小数）。
    pub percentage: f64,
    /// 电池状态。
    pub state: State,
    /// 预计剩余使用时间（秒，0 = 未知或不在放电）。
    pub time_to_empty: i64,
    /// 预计充满时间（秒，0 = 未知或不在充电）。
    pub time_to_full: i64,
    /// 是否接着外部电源。
    pub ac_online: bool,
    /// 低电量警告级别。
    pub warning: Warning,
}

/// 功率的指数平滑。状态或量纲变化时重新开始。
#[derive(Debug, Default)]
pub struct Estimator {
    key: Option<(State, Unit)>,
    rate: Option<f64>,
}

impl Estimator {
    fn update(&mut self, state: State, unit: Unit, sample: Option<f64>) -> Option<f64> {
        if self.key != Some((state, unit)) {
            self.key = Some((state, unit));
            self.rate = None;
        }
        if let Some(sample) = sample {
            self.rate = Some(match self.rate {
                Some(old) => old + RATE_SMOOTHING * (sample - old),
                None => sample,
            });
        }
        self.rate
    }
}

fn sum_reserves(batteries: &[Battery], unit: Unit) -> Option<Reserve> {
    let mut total = Reserve {
        now: 0.0,
        full: 0.0,
        rate: Some(0.0),
    };
    for b in batteries {
        let r = b.reserve(unit)?;
        total.now += r.now;
        total.full += r.full;
        total.rate = match (total.rate, r.rate) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
    }
    Some(total)
}

fn sane_secs(secs: f64) -> i64 {
    if secs.is_finite() && secs > 0.0 && secs <= MAX_ESTIMATE_SECS {
        // 按分钟取整，避免每次轮询都因为几秒的抖动发信号。
        ((secs / 60.0).round() * 60.0) as i64
    } else {
        0
    }
}

/// 由读数汇总出对外状态。
pub fn summarize(supplies: &Supplies, est: &mut Estimator) -> Status {
    let batteries = &supplies.batteries;
    if batteries.is_empty() {
        return Status {
            ac_online: supplies.ac_online.unwrap_or(true),
            ..Status::default()
        };
    }

    // 电量：多块电池按满电容量加权（缺容量信息时取平均）。
    let readings: Vec<(f64, Option<f64>)> = batteries
        .iter()
        .filter_map(|b| Some((b.percentage()?, b.weight())))
        .collect();
    let percentage = if readings.is_empty() {
        0.0
    } else if readings.iter().all(|(_, w)| w.is_some()) {
        let total: f64 = readings.iter().map(|(_, w)| w.unwrap_or(0.0)).sum();
        readings
            .iter()
            .map(|(p, w)| p * w.unwrap_or(0.0))
            .sum::<f64>()
            / total
    } else {
        readings.iter().map(|(p, _)| p).sum::<f64>() / readings.len() as f64
    };
    let percentage = (percentage * 10.0).round() / 10.0;

    // 状态
    let any = |s: State| batteries.iter().any(|b| b.status == s);
    let mut state = if any(State::Charging) {
        State::Charging
    } else if any(State::Discharging) {
        State::Discharging
    } else if batteries.iter().all(|b| b.status == State::Full) {
        State::Full
    } else if any(State::NotCharging) {
        State::NotCharging
    } else {
        State::Unknown
    };
    // 有的驱动在接电且接近满电时报告 Unknown。
    if state == State::Unknown {
        match supplies.ac_online {
            Some(true) if percentage >= 99.0 => state = State::Full,
            Some(false) => state = State::Discharging,
            _ => {}
        }
    }
    let ac_online = supplies.ac_online.unwrap_or(matches!(
        state,
        State::Charging | State::Full | State::NotCharging
    ));

    // 剩余/充满时间
    let (mut time_to_empty, mut time_to_full) = (0, 0);
    let single = (batteries.len() == 1).then(|| &batteries[0]);
    let driver_time = |f: fn(&Battery) -> Option<f64>| single.and_then(f).filter(|t| *t > 0.0);
    let totals = [Unit::Energy, Unit::Charge]
        .into_iter()
        .find_map(|u| Some((u, sum_reserves(batteries, u)?)));
    let rate = totals.and_then(|(unit, r)| est.update(state, unit, r.rate.filter(|r| *r > 0.0)));
    match state {
        State::Discharging => {
            time_to_empty = match driver_time(|b| b.time_to_empty) {
                Some(t) => sane_secs(t),
                None => match (totals, rate) {
                    (Some((_, r)), Some(rate)) => sane_secs(r.now / rate * 3600.0),
                    _ => 0,
                },
            };
        }
        State::Charging => {
            time_to_full = match driver_time(|b| b.time_to_full) {
                Some(t) => sane_secs(t),
                None => match (totals, rate) {
                    (Some((_, r)), Some(rate)) => sane_secs((r.full - r.now) / rate * 3600.0),
                    _ => 0,
                },
            };
        }
        _ => {}
    }

    let warning = if state == State::Discharging || (!ac_online && state == State::Unknown) {
        if percentage <= CRITICAL_THRESHOLD {
            Warning::Critical
        } else if percentage <= LOW_THRESHOLD {
            Warning::Low
        } else {
            Warning::None
        }
    } else {
        Warning::None
    };

    Status {
        has_battery: true,
        percentage,
        state,
        time_to_empty,
        time_to_full,
        ac_online,
        warning,
    }
}

/// 一次刷新带来的变化。
#[derive(Debug, Default)]
pub struct Update {
    /// 变化了的属性。
    pub changed: Changed,
    /// 警告级别升高（需要发 `LowBattery`）。
    pub alert: Option<(Warning, f64)>,
}

fn diff(old: &Status, new: &Status) -> Update {
    let mut changed: Changed = vec![];
    if old.has_battery != new.has_battery {
        changed.push(("HasBattery", bus::owned(new.has_battery)));
    }
    if old.percentage != new.percentage {
        changed.push(("Percentage", bus::owned(new.percentage)));
    }
    if old.state != new.state {
        changed.push(("State", bus::owned(new.state.as_str())));
    }
    if old.time_to_empty != new.time_to_empty {
        changed.push(("TimeToEmpty", bus::owned(new.time_to_empty)));
    }
    if old.time_to_full != new.time_to_full {
        changed.push(("TimeToFull", bus::owned(new.time_to_full)));
    }
    if old.ac_online != new.ac_online {
        changed.push(("AcOnline", bus::owned(new.ac_online)));
    }
    if old.warning != new.warning {
        changed.push(("WarningLevel", bus::owned(new.warning.as_str())));
    }
    let alert = (new.warning > old.warning).then_some((new.warning, new.percentage));
    Update { changed, alert }
}

struct Inner {
    status: Status,
    estimator: Estimator,
}

/// 电源状态。接口对象与后台任务共享。
pub struct Power {
    sysfs: Sysfs,
    inner: Mutex<Inner>,
}

impl Power {
    /// 读取一次初始状态。
    pub fn new(sysfs: Sysfs) -> Self {
        let mut estimator = Estimator::default();
        let status = summarize(&read_supplies(&sysfs), &mut estimator);
        if status.has_battery {
            tracing::info!(
                "电池 {:.0}%（{}），外部电源{}",
                status.percentage,
                status.state.as_str(),
                if status.ac_online { "在线" } else { "离线" }
            );
        } else {
            tracing::info!("没有电池");
        }
        Power {
            sysfs,
            inner: Mutex::new(Inner { status, estimator }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 当前状态。
    pub fn status(&self) -> Status {
        self.lock().status.clone()
    }

    /// 重新读取 sysfs，返回变化。
    pub fn refresh(&self) -> Update {
        let supplies = read_supplies(&self.sysfs);
        let mut inner = self.lock();
        let new = summarize(&supplies, &mut inner.estimator);
        let update = diff(&inner.status, &new);
        inner.status = new;
        update
    }
}

/// 刷新并发出信号。
pub async fn refresh_and_notify(power: &Power, conn: &Connection) {
    let update = power.refresh();
    bus::emit_changed(conn, PATH, INTERFACE, &update.changed).await;
    if let Some((level, percentage)) = update.alert {
        tracing::info!("电量 {percentage:.0}%：{}", level.as_str());
        let result = match SignalEmitter::new(conn, PATH) {
            Ok(emitter) => PowerIface::low_battery(&emitter, level.as_str(), percentage).await,
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            tracing::warn!("发送 LowBattery 失败: {e}");
        }
    }
}

/// 后台轮询任务。
pub async fn run_poller(power: Arc<Power>, conn: Connection) {
    loop {
        async_io::Timer::after(POLL_INTERVAL).await;
        refresh_and_notify(&power, &conn).await;
    }
}

/// D-Bus 接口 `org.loong.System1.Power`。
pub struct PowerIface(pub Arc<Power>);

#[interface(name = "org.loong.System1.Power")]
impl PowerIface {
    /// 是否有电池（台式机、QEMU 没有）。
    #[zbus(property)]
    fn has_battery(&self) -> bool {
        self.0.status().has_battery
    }

    /// 电量 0.0–100.0。
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.0.status().percentage
    }

    /// `charging` / `discharging` / `full` / `not-charging` / `unknown`。
    #[zbus(property)]
    fn state(&self) -> String {
        self.0.status().state.as_str().into()
    }

    /// 预计剩余使用时间（秒），0 表示未知或不在放电。
    #[zbus(property)]
    fn time_to_empty(&self) -> i64 {
        self.0.status().time_to_empty
    }

    /// 预计充满时间（秒），0 表示未知或不在充电。
    #[zbus(property)]
    fn time_to_full(&self) -> i64 {
        self.0.status().time_to_full
    }

    /// 是否接着外部电源（充电器、USB）。
    #[zbus(property)]
    fn ac_online(&self) -> bool {
        self.0.status().ac_online
    }

    /// 低电量警告：`none` / `low` / `critical`（只在放电时出现）。
    #[zbus(property)]
    fn warning_level(&self) -> String {
        self.0.status().warning.as_str().into()
    }

    /// 低电量阈值（%）。
    #[zbus(property)]
    fn low_threshold(&self) -> f64 {
        LOW_THRESHOLD
    }

    /// 极低电量阈值（%）。
    #[zbus(property)]
    fn critical_threshold(&self) -> f64 {
        CRITICAL_THRESHOLD
    }

    /// 立即重新读取电源状态（有变化会发 `PropertiesChanged`）。对所有人开放。
    async fn refresh(&self, #[zbus(connection)] conn: &Connection) {
        refresh_and_notify(&self.0, conn).await;
    }

    /// 放电中电量降到低电量（`low`）或极低电量（`critical`）阈值。每次升级只发一次，
    /// 接上电源后重新计算。
    #[zbus(signal)]
    pub async fn low_battery(
        emitter: &SignalEmitter<'_>,
        level: &str,
        percentage: f64,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysfs::testutil::TempDir;

    fn supply(t: &TempDir, name: &str, files: &[(&str, &str)]) {
        for (file, content) in files {
            t.write(&format!("class/power_supply/{name}/{file}"), content);
        }
    }

    fn status_of(t: &TempDir) -> Status {
        summarize(
            &read_supplies(&Sysfs::new(t.path())),
            &mut Estimator::default(),
        )
    }

    #[test]
    fn laptop_style_energy_battery_discharging() {
        let t = TempDir::new("pw-laptop");
        supply(
            &t,
            "BAT0",
            &[
                ("type", "Battery\n"),
                ("present", "1\n"),
                ("status", "Discharging\n"),
                ("capacity", "42\n"),
                ("energy_now", "21000000\n"),
                ("energy_full", "50000000\n"),
                ("power_now", "7000000\n"),
            ],
        );
        supply(&t, "AC", &[("type", "Mains\n"), ("online", "0\n")]);
        let s = status_of(&t);
        assert!(s.has_battery);
        assert_eq!(s.percentage, 42.0);
        assert_eq!(s.state, State::Discharging);
        assert!(!s.ac_online);
        assert_eq!(s.time_to_empty, 3 * 3600, "21 Wh / 7 W = 3 h");
        assert_eq!(s.time_to_full, 0);
        assert_eq!(s.warning, Warning::None);
    }

    #[test]
    fn phone_style_charge_battery_with_negative_current_and_usb() {
        let t = TempDir::new("pw-phone");
        supply(
            &t,
            "battery",
            &[
                ("type", "Battery"),
                ("status", "Charging"),
                ("charge_now", "4000000"),
                ("charge_full", "8000000"),
                ("current_now", "-2000000"),
                ("voltage_now", "3900000"),
            ],
        );
        supply(&t, "usb", &[("type", "USB"), ("online", "1")]);
        let s = status_of(&t);
        assert_eq!(s.percentage, 50.0, "没有 capacity 时用 charge 计算");
        assert_eq!(s.state, State::Charging);
        assert!(s.ac_online);
        assert_eq!(s.time_to_full, 2 * 3600, "4 Ah / 2 A = 2 h");
        assert_eq!(s.time_to_empty, 0);
    }

    #[test]
    fn driver_provided_times_win() {
        let t = TempDir::new("pw-driver-time");
        supply(
            &t,
            "BAT1",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("capacity", "80"),
                ("time_to_empty_now", "5410"),
                ("energy_now", "1"),
                ("energy_full", "2"),
                ("power_now", "1"),
            ],
        );
        assert_eq!(status_of(&t).time_to_empty, 5400, "按分钟取整");
    }

    #[test]
    fn odd_and_missing_files() {
        let t = TempDir::new("pw-odd");
        // capacity 超过 100、status 未知、没有任何能量/电流信息、online 文件是坏的
        supply(
            &t,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Unknown"),
                ("capacity", "104"),
            ],
        );
        supply(&t, "ADP1", &[("type", "Mains"), ("online", "garbage")]);
        // 外设电池（触控笔）、不在位的电池、未知类型都应忽略
        supply(
            &t,
            "hid-stylus-battery",
            &[("type", "Battery"), ("scope", "Device"), ("capacity", "3")],
        );
        supply(&t, "BAT9", &[("type", "Battery"), ("present", "0")]);
        supply(&t, "ups", &[("type", "UPS"), ("online", "1")]);
        // 只有一个空目录的条目
        std::fs::create_dir_all(t.path().join("class/power_supply/empty")).unwrap();

        let supplies = read_supplies(&Sysfs::new(t.path()));
        assert_eq!(supplies.batteries.len(), 1);
        assert_eq!(supplies.ac_online, None);
        let s = summarize(&supplies, &mut Estimator::default());
        assert_eq!(s.percentage, 100.0);
        assert_eq!(s.state, State::Unknown);
        assert_eq!((s.time_to_empty, s.time_to_full), (0, 0));
        // 没有外部电源信息、状态未知：不认为接着电源，但也不报低电量（电量满）
        assert!(!s.ac_online);
        assert_eq!(s.warning, Warning::None);
    }

    #[test]
    fn battery_without_any_level_info() {
        let t = TempDir::new("pw-nolevel");
        supply(&t, "BAT0", &[("type", "Battery"), ("status", "Full")]);
        supply(&t, "AC0", &[("type", "Mains"), ("online", "1")]);
        let s = status_of(&t);
        assert!(s.has_battery);
        assert_eq!(s.percentage, 0.0);
        assert_eq!(s.state, State::Full);
        assert_eq!(s.warning, Warning::None, "接着电源不报警");
    }

    #[test]
    fn unknown_status_inferred_from_ac() {
        let t = TempDir::new("pw-infer");
        supply(
            &t,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Unknown"),
                ("capacity", "100"),
            ],
        );
        supply(&t, "AC", &[("type", "Mains"), ("online", "1")]);
        assert_eq!(status_of(&t).state, State::Full);
        supply(&t, "AC", &[("online", "0")]);
        supply(&t, "BAT0", &[("capacity", "12")]);
        let s = status_of(&t);
        assert_eq!(s.state, State::Discharging);
        assert_eq!(s.warning, Warning::Low);
    }

    #[test]
    fn no_power_supply_class_at_all() {
        let t = TempDir::new("pw-none");
        let s = status_of(&t);
        assert!(!s.has_battery);
        assert!(s.ac_online, "没有电池也没有电源信息时视为接着电源");
        assert_eq!(s.warning, Warning::None);
    }

    #[test]
    fn two_batteries_weighted_by_capacity() {
        let t = TempDir::new("pw-two");
        supply(
            &t,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("capacity", "100"),
                ("energy_now", "30000000"),
                ("energy_full", "30000000"),
                ("power_now", "5000000"),
            ],
        );
        supply(
            &t,
            "BAT1",
            &[
                ("type", "Battery"),
                ("status", "Unknown"),
                ("capacity", "0"),
                ("energy_now", "0"),
                ("energy_full", "10000000"),
                ("power_now", "0"),
            ],
        );
        let s = status_of(&t);
        assert_eq!(s.percentage, 75.0);
        assert_eq!(s.state, State::Discharging);
        assert_eq!(s.time_to_empty, 6 * 3600, "30 Wh / 5 W");
    }

    #[test]
    fn rate_is_smoothed_and_reset_on_state_change() {
        let mut est = Estimator::default();
        assert_eq!(
            est.update(State::Discharging, Unit::Energy, Some(10.0)),
            Some(10.0)
        );
        let r = est
            .update(State::Discharging, Unit::Energy, Some(20.0))
            .unwrap();
        assert!((r - 13.0).abs() < 1e-9);
        assert_eq!(est.update(State::Discharging, Unit::Energy, None), Some(r));
        assert_eq!(
            est.update(State::Charging, Unit::Energy, Some(5.0)),
            Some(5.0)
        );
    }

    #[test]
    fn refresh_reports_changes_and_low_battery_once() {
        let t = TempDir::new("pw-refresh");
        let bat = [
            ("type", "Battery"),
            ("status", "Discharging"),
            ("capacity", "40"),
        ];
        supply(&t, "BAT0", &bat);
        let power = Power::new(Sysfs::new(t.path()));
        assert!(power.refresh().changed.is_empty());

        supply(&t, "BAT0", &[("capacity", "14")]);
        let u = power.refresh();
        let names: Vec<_> = u.changed.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Percentage", "WarningLevel"]);
        assert_eq!(u.alert, Some((Warning::Low, 14.0)));

        supply(&t, "BAT0", &[("capacity", "13")]);
        assert_eq!(power.refresh().alert, None, "同一级别不重复提醒");

        supply(&t, "BAT0", &[("capacity", "4")]);
        assert_eq!(power.refresh().alert, Some((Warning::Critical, 4.0)));

        supply(&t, "BAT0", &[("status", "Charging")]);
        let u = power.refresh();
        assert_eq!(u.alert, None);
        assert_eq!(power.status().warning, Warning::None);
        assert_eq!(power.status().state, State::Charging);
        assert!(power.status().ac_online, "没有 AC 条目时由充电状态推断");
    }
}
