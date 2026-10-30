//! 屏幕背光：`/org/loong/System1/Backlight`。
//!
//! - **设备选择**：`/sys/class/backlight/*` 中按类型优先级 firmware > platform > raw 选择
//!   （与 systemd-backlight、brightnessctl 一致：firmware/platform 接口通常已经做了合理的
//!   映射，raw 是直接写 PWM 占空比）。环境变量 `LOONG_BACKLIGHT` 可以指定设备名。
//! - **感知亮度**：对外的 `Brightness` 是 0.0–1.0 的“感知亮度”，与 raw 值之间用 γ≈2.2
//!   的幂函数映射：滑块拖到一半时人眼看到的是“一半亮”，而不是 raw 的一半（raw 一半看起来
//!   接近最亮）。级数很少的设备（≤ 32 级，例如 ACPI 的 0–15）用线性映射，否则低端几级会
//!   挤在一起。
//! - **保持可见**：亮度 0.0 对应的是“最低可见亮度”（raw ≥ 1，约为最大值的 0.5%），不会把
//!   屏幕关黑。只有设置了 `LOONG_BACKLIGHT_ALLOW_OFF=1` 时 0.0 才对应 raw 0。
//! - **平滑过渡**：`SetBrightness(value, animate=true)` 用临界阻尼弹簧（loong-motion）在
//!   感知空间里过渡到目标值（约 0.35 秒），过渡中再次设置会从当前位置和速度继续，拖动滑块
//!   时不会跳。

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use event_listener::Event;
use loong_motion::{Spring, SpringAnimation};
use zbus::{Connection, interface, message::Header, object_server::SignalEmitter};

use crate::{
    access,
    bus::{self, Changed},
    error::Error,
    sysfs::{self, Sysfs},
};

/// 对象路径。
pub const PATH: &str = "/org/loong/System1/Backlight";
/// 接口名。
pub const INTERFACE: &str = "org.loong.System1.Backlight";

/// 环境变量：指定背光设备名（`/sys/class/backlight/<名称>`）。
pub const DEVICE_ENV: &str = "LOONG_BACKLIGHT";
/// 环境变量：设为 1 时允许亮度 0.0 对应 raw 0（可能关掉背光）。
pub const ALLOW_OFF_ENV: &str = "LOONG_BACKLIGHT_ALLOW_OFF";

/// 感知亮度与 raw 之间的幂指数。
pub const GAMMA: f64 = 2.2;
/// raw 级数不超过这个值时使用线性映射。
pub const LINEAR_MAX_RAW: u32 = 32;
/// 最低可见亮度占 raw 最大值的比例。
const MIN_VISIBLE_FRACTION: f64 = 0.005;
/// 过渡动画：临界阻尼（不过冲，亮度不会“闪一下”），约 0.35 秒到位。
const RAMP: Spring = Spring::new(0.25, 1.0);
/// 动画帧间隔。
const FRAME: Duration = Duration::from_millis(16);

/// 背光接口类型（`/sys/class/backlight/*/type`），按优先级排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// 固件接口（例如 ACPI）。
    Firmware,
    /// 平台驱动。
    Platform,
    /// 直接控制硬件（PWM 等）。
    Raw,
    /// 未知类型。
    Unknown,
}

impl Kind {
    fn parse(s: Option<&str>) -> Kind {
        match s {
            Some("firmware") => Kind::Firmware,
            Some("platform") => Kind::Platform,
            Some("raw") => Kind::Raw,
            _ => Kind::Unknown,
        }
    }
}

/// 一个背光设备。
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// 设备名（`/sys/class/backlight/` 下的目录名）。
    pub name: String,
    /// 设备目录。
    pub dir: PathBuf,
    /// 接口类型。
    pub kind: Kind,
    /// `max_brightness`。
    pub max_raw: u32,
}

impl Device {
    fn read(name: String, dir: PathBuf) -> Option<Device> {
        let max_raw = sysfs::read_num::<u32>(&dir.join("max_brightness")).filter(|&m| m > 0)?;
        let kind = Kind::parse(sysfs::read_string(&dir.join("type")).as_deref());
        Some(Device {
            name,
            dir,
            kind,
            max_raw,
        })
    }

    fn read_raw(&self) -> Option<u32> {
        sysfs::read_num(&self.dir.join("brightness"))
    }

    fn write_raw(&self, raw: u32) -> std::io::Result<()> {
        sysfs::write_string(&self.dir.join("brightness"), &raw.to_string())
    }
}

/// 选择背光设备：`preferred` 指定名称时只用它；否则按类型优先级、再按名称选第一个。
/// 没有 `max_brightness`（或为 0）的条目会被忽略。
pub fn discover(sysfs: &Sysfs, preferred: Option<&str>) -> Option<Device> {
    let mut devices: Vec<Device> = sysfs
        .class_entries("backlight")
        .into_iter()
        .filter(|(name, _)| preferred.is_none_or(|p| p == name))
        .filter_map(|(name, dir)| Device::read(name, dir))
        .collect();
    devices.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    devices.into_iter().next()
}

/// 感知亮度（0.0–1.0）与 raw 值之间的映射。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    /// raw 最大值。
    pub max_raw: u32,
    /// 感知亮度 0.0 对应的 raw 值。
    pub min_raw: u32,
    /// 幂指数（线性映射时为 1）。
    pub gamma: f64,
}

impl Curve {
    /// 根据 raw 最大值生成映射。`allow_off` 为 false 时最低亮度保持可见（raw ≥ 1）。
    pub fn new(max_raw: u32, allow_off: bool) -> Curve {
        let gamma = if max_raw > LINEAR_MAX_RAW { GAMMA } else { 1.0 };
        let min_raw = if allow_off {
            0
        } else {
            ((f64::from(max_raw) * MIN_VISIBLE_FRACTION).round() as u32).clamp(1, max_raw.max(1))
        };
        Curve {
            max_raw,
            min_raw: min_raw.min(max_raw),
            gamma,
        }
    }

    fn span(&self) -> f64 {
        f64::from(self.max_raw - self.min_raw)
    }

    /// 感知亮度 → raw。
    pub fn to_raw(self, value: f64) -> u32 {
        let v = if value.is_nan() {
            0.0
        } else {
            value.clamp(0.0, 1.0)
        };
        let raw = f64::from(self.min_raw) + self.span() * v.powf(self.gamma);
        (raw.round() as u32).clamp(self.min_raw, self.max_raw)
    }

    /// raw → 感知亮度。
    pub fn to_value(self, raw: u32) -> f64 {
        if self.span() <= 0.0 {
            return 1.0;
        }
        if raw <= self.min_raw {
            return 0.0;
        }
        let linear =
            (f64::from(raw.min(self.max_raw) - self.min_raw) / self.span()).clamp(0.0, 1.0);
        linear.powf(1.0 / self.gamma)
    }
}

/// 背光的启动选项（来自环境变量）。
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// 指定设备名。
    pub device: Option<String>,
    /// 允许亮度 0.0 对应 raw 0。
    pub allow_off: bool,
}

impl Options {
    /// 读取 `LOONG_BACKLIGHT` 与 `LOONG_BACKLIGHT_ALLOW_OFF`。
    pub fn from_env() -> Self {
        Options {
            device: std::env::var(DEVICE_ENV).ok().filter(|s| !s.is_empty()),
            allow_off: std::env::var(ALLOW_OFF_ENV).is_ok_and(|v| v == "1"),
        }
    }
}

struct State {
    device: Option<Device>,
    curve: Curve,
    /// 对外报告的亮度。过渡动画中是目标值（滑块不会被拉回去）。
    value: f64,
    /// 最后一次写入或读到的 raw。
    raw: u32,
    /// 进行中的过渡动画（感知空间）。
    anim: Option<SpringAnimation>,
}

/// 背光控制。接口对象与后台动画任务共享。
pub struct Backlight {
    state: Mutex<State>,
    /// 有新动画时唤醒动画任务。
    wake: Event,
    epoch: Instant,
}

impl Backlight {
    /// 发现设备并读取当前亮度。没有背光设备时 `available()` 为 false。
    pub fn new(sysfs: &Sysfs, options: &Options) -> Self {
        let device = discover(sysfs, options.device.as_deref());
        let (curve, raw) = match &device {
            Some(dev) => {
                let curve = Curve::new(dev.max_raw, options.allow_off);
                (curve, dev.read_raw().unwrap_or(dev.max_raw))
            }
            None => (Curve::new(1, false), 1),
        };
        match &device {
            Some(dev) => tracing::info!(
                "背光设备 {}（{:?}，max_brightness={}，当前 {raw}）",
                dev.name,
                dev.kind,
                dev.max_raw
            ),
            None => tracing::info!("没有背光设备"),
        }
        Backlight {
            state: Mutex::new(State {
                device,
                curve,
                value: curve.to_value(raw),
                raw,
                anim: None,
            }),
            wake: Event::new(),
            epoch: Instant::now(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    /// 是否有背光设备。
    pub fn available(&self) -> bool {
        self.lock().device.is_some()
    }

    /// 当前（或过渡目标）感知亮度。
    pub fn value(&self) -> f64 {
        self.lock().value
    }

    /// raw 最大值（没有设备时为 0）。
    pub fn max_raw(&self) -> u32 {
        self.lock().device.as_ref().map_or(0, |d| d.max_raw)
    }

    /// 设备名（没有设备时为空）。
    pub fn device_name(&self) -> String {
        self.lock()
            .device
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_default()
    }

    /// 设置感知亮度。返回对外报告的 `Brightness` 是否变化。
    pub fn set(&self, value: f64, animate: bool) -> Result<bool, Error> {
        if !value.is_finite() {
            return Err(Error::InvalidArgs("亮度必须是 0.0–1.0 之间的数".into()));
        }
        let target = value.clamp(0.0, 1.0);
        let now = self.now();
        let mut st = self.lock();
        let Some(device) = st.device.clone() else {
            return Err(Error::NotSupported("没有背光设备".into()));
        };
        let changed = st.value != target;
        if animate {
            match &mut st.anim {
                Some(anim) => anim.retarget(target, now),
                None => {
                    if !changed {
                        return Ok(false);
                    }
                    let from = st.value;
                    st.anim = Some(SpringAnimation::new(RAMP, from, target, 0.0, now));
                }
            }
            st.value = target;
            drop(st);
            self.wake.notify(usize::MAX);
        } else {
            st.anim = None;
            let raw = st.curve.to_raw(target);
            if raw != st.raw {
                device
                    .write_raw(raw)
                    .map_err(|e| Error::Failed(format!("写入 {} 失败: {e}", device.name)))?;
                st.raw = raw;
            }
            st.value = target;
        }
        Ok(changed)
    }

    /// 是否有进行中的过渡动画。
    fn animating(&self) -> bool {
        self.lock().anim.is_some()
    }

    /// 推进一帧动画。返回动画是否还要继续。
    fn step(&self) -> bool {
        let now = self.now();
        let mut st = self.lock();
        let Some(anim) = st.anim else {
            return false;
        };
        let done = anim.is_done(now);
        let raw = st.curve.to_raw(anim.value(now));
        if raw != st.raw {
            let Some(device) = &st.device else {
                st.anim = None;
                return false;
            };
            if let Err(e) = device.write_raw(raw) {
                tracing::warn!("写入背光 {} 失败: {e}", device.name);
                st.anim = None;
                return false;
            }
            st.raw = raw;
        }
        if done {
            st.anim = None;
        }
        !done
    }

    /// 重新读取 raw（亮度被别处改了，例如笔记本的亮度热键）。返回 `Brightness` 是否变化。
    pub fn reload(&self) -> bool {
        let mut st = self.lock();
        if st.anim.is_some() {
            return false;
        }
        let Some(raw) = st.device.as_ref().and_then(Device::read_raw) else {
            return false;
        };
        if raw == st.raw {
            return false;
        }
        st.raw = raw;
        st.value = st.curve.to_value(raw);
        true
    }
}

/// 后台动画任务：等到有过渡动画时按帧推进，结束后继续等待。
pub async fn run_animator(backlight: Arc<Backlight>) {
    loop {
        let listener = backlight.wake.listen();
        if !backlight.animating() {
            listener.await;
            continue;
        }
        while backlight.step() {
            async_io::Timer::after(FRAME).await;
        }
    }
}

/// 外部修改后重新读取，有变化时发 `PropertiesChanged`。
pub async fn reload_and_notify(backlight: &Backlight, conn: &Connection) {
    if backlight.reload() {
        let changed: Changed = vec![("Brightness", bus::owned(backlight.value()))];
        bus::emit_changed(conn, PATH, INTERFACE, &changed).await;
    }
}

/// D-Bus 接口 `org.loong.System1.Backlight`。
pub struct BacklightIface(pub Arc<Backlight>);

#[interface(name = "org.loong.System1.Backlight")]
impl BacklightIface {
    /// 是否找到了背光设备。为 false 时 `SetBrightness` 返回 `NotSupported`。
    #[zbus(property)]
    fn available(&self) -> bool {
        self.0.available()
    }

    /// 感知亮度 0.0–1.0（过渡动画中为目标值）。
    #[zbus(property)]
    fn brightness(&self) -> f64 {
        self.0.value()
    }

    /// 设备的 raw 最大值（`max_brightness`），没有设备时为 0。
    #[zbus(property)]
    fn max_raw(&self) -> u32 {
        self.0.max_raw()
    }

    /// 背光设备名。
    #[zbus(property)]
    fn device(&self) -> String {
        self.0.device_name()
    }

    /// 设置感知亮度（0.0–1.0，超出范围会被截断）。`animate` 为 true 时平滑过渡。
    /// 仅限 root 与 loong。
    async fn set_brightness(
        &self,
        value: f64,
        animate: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), Error> {
        access::require_privileged(conn, &hdr).await?;
        if self.0.set(value, animate)? {
            self.brightness_changed(&emitter).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysfs::testutil::TempDir;

    fn fake_device(t: &TempDir, name: &str, kind: &str, max: u32, cur: u32) {
        t.write(
            &format!("class/backlight/{name}/type"),
            &format!("{kind}\n"),
        );
        t.write(
            &format!("class/backlight/{name}/max_brightness"),
            &format!("{max}\n"),
        );
        t.write(
            &format!("class/backlight/{name}/brightness"),
            &format!("{cur}\n"),
        );
    }

    #[test]
    fn discovery_prefers_firmware_then_platform_then_raw() {
        let t = TempDir::new("bl-discover");
        fake_device(&t, "a_raw", "raw", 255, 10);
        fake_device(&t, "b_platform", "platform", 100, 10);
        fake_device(&t, "c_firmware", "firmware", 15, 10);
        let sysfs = Sysfs::new(t.path());
        assert_eq!(discover(&sysfs, None).unwrap().name, "c_firmware");
        assert_eq!(discover(&sysfs, Some("a_raw")).unwrap().name, "a_raw");
        assert!(discover(&sysfs, Some("nope")).is_none());

        // 同类型按名称排序；坏条目（没有 max_brightness 或为 0）被忽略。
        let t = TempDir::new("bl-discover2");
        fake_device(&t, "panel1", "raw", 1023, 1);
        fake_device(&t, "panel0", "raw", 0, 0);
        t.write("class/backlight/broken/type", "firmware");
        let d = discover(&Sysfs::new(t.path()), None).unwrap();
        assert_eq!(
            (d.name.as_str(), d.kind, d.max_raw),
            ("panel1", Kind::Raw, 1023)
        );
    }

    #[test]
    fn no_backlight_dir_means_unavailable() {
        let t = TempDir::new("bl-none");
        let bl = Backlight::new(&Sysfs::new(t.path()), &Options::default());
        assert!(!bl.available());
        assert_eq!(bl.max_raw(), 0);
        assert!(matches!(bl.set(0.5, false), Err(Error::NotSupported(_))));
    }

    #[test]
    fn curve_is_perceptual_and_keeps_screen_visible() {
        let c = Curve::new(1000, false);
        assert_eq!(c.gamma, GAMMA);
        assert_eq!(c.min_raw, 5);
        assert_eq!(c.to_raw(0.0), 5);
        assert_eq!(c.to_raw(1.0), 1000);
        // 感知一半远低于 raw 一半。
        let half = c.to_raw(0.5);
        assert!((200..240).contains(&half), "0.5 → {half}");
        assert!((c.to_value(half) - 0.5).abs() < 0.01);
        // 越界与 NaN
        assert_eq!(c.to_raw(-1.0), 5);
        assert_eq!(c.to_raw(7.0), 1000);
        assert_eq!(c.to_raw(f64::NAN), 5);
        assert_eq!(c.to_value(0), 0.0);
        assert_eq!(c.to_value(5000), 1.0);
        // 单调
        let mut last = 0;
        for i in 0..=100 {
            let raw = c.to_raw(f64::from(i) / 100.0);
            assert!(raw >= last);
            last = raw;
        }
    }

    #[test]
    fn curve_roundtrips_every_raw_level() {
        for max in [1, 7, 15, 32, 100, 255, 1023, 4095, 96000] {
            for allow_off in [false, true] {
                let c = Curve::new(max, allow_off);
                assert!(c.min_raw <= c.max_raw);
                if !allow_off {
                    assert!(c.min_raw >= 1, "max={max}");
                }
                let step = (max / 500).max(1);
                for raw in (c.min_raw..=max).step_by(step as usize) {
                    assert_eq!(c.to_raw(c.to_value(raw)), raw, "max={max} raw={raw}");
                }
            }
        }
    }

    #[test]
    fn few_levels_use_linear_mapping_and_allow_off_reaches_zero() {
        let c = Curve::new(15, false);
        assert_eq!(c.gamma, 1.0);
        assert_eq!(c.min_raw, 1);
        assert_eq!(c.to_raw(0.5), 8);
        let c = Curve::new(255, true);
        assert_eq!(c.min_raw, 0);
        assert_eq!(c.to_raw(0.0), 0);
    }

    #[test]
    fn set_writes_raw_and_animation_ramps_to_target() {
        let t = TempDir::new("bl-set");
        fake_device(&t, "panel", "raw", 1000, 1000);
        let bl = Backlight::new(&Sysfs::new(t.path()), &Options::default());
        assert_eq!(bl.value(), 1.0);

        // 立即设置
        assert!(bl.set(0.5, false).unwrap());
        let expected = Curve::new(1000, false).to_raw(0.5).to_string();
        assert_eq!(t.read("class/backlight/panel/brightness"), expected);
        assert!(!bl.set(0.5, false).unwrap(), "相同的值不算变化");
        assert!(matches!(
            bl.set(f64::NAN, false),
            Err(Error::InvalidArgs(_))
        ));

        // 过渡：属性立即报告目标值，raw 逐帧靠近，最终精确到达
        assert!(bl.set(0.0, true).unwrap());
        assert_eq!(bl.value(), 0.0);
        let mut seen = vec![];
        let start = Instant::now();
        while bl.step() {
            seen.push(
                t.read("class/backlight/panel/brightness")
                    .parse::<u32>()
                    .unwrap(),
            );
            std::thread::sleep(FRAME);
            assert!(start.elapsed() < Duration::from_secs(3), "动画没有结束");
        }
        assert_eq!(t.read("class/backlight/panel/brightness"), "5");
        assert!(seen.len() >= 3, "应该有中间帧: {seen:?}");
        assert!(seen.windows(2).all(|w| w[1] <= w[0]), "单调下降: {seen:?}");
    }

    #[test]
    fn reload_picks_up_external_changes() {
        let t = TempDir::new("bl-reload");
        fake_device(&t, "acpi_video0", "firmware", 15, 15);
        let bl = Backlight::new(&Sysfs::new(t.path()), &Options::default());
        assert!(!bl.reload());
        t.write("class/backlight/acpi_video0/brightness", "8\n");
        assert!(bl.reload());
        assert!((bl.value() - 0.5).abs() < 1e-9);
    }
}
