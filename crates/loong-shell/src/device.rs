//! 设备描述（`/usr/share/loong/device.toml`）。shell 只依据这里的数据适配机型，
//! 不写死任何具体设备。

use serde::Deserialize;

/// 默认设备描述文件路径。
pub const DEFAULT_PATH: &str = "/usr/share/loong/device.toml";

/// 尺寸类：决定使用平板还是手机布局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SizeClass {
    /// 平板（v1）。
    #[default]
    Regular,
    /// 手机（v2 预留）。
    Compact,
}

/// 屏幕参数。
#[derive(Debug, Clone, Deserialize)]
pub struct Screen {
    /// 物理像素宽。
    pub width: u32,
    /// 物理像素高。
    pub height: u32,
    /// 缩放（物理像素 / 逻辑点）。缺省时由物理尺寸推算。
    pub scale: Option<f64>,
    /// 屏幕对角线英寸，用于推算缩放。
    pub diagonal_inch: Option<f64>,
    /// 刷新率。
    #[allow(dead_code, reason = "真机后端按此选择显示模式")]
    #[serde(default = "default_refresh")]
    pub refresh_hz: u32,
    /// 屏幕圆角（逻辑点），全屏应用的圆角与之一致。
    #[serde(default)]
    pub corner_radius: f64,
}

fn default_refresh() -> u32 {
    60
}

/// 设备描述。
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceConfig {
    /// 设备名称。
    pub name: String,
    /// 尺寸类。
    #[allow(dead_code, reason = "v2 手机布局使用")]
    #[serde(default)]
    pub size_class: SizeClass,
    /// 屏幕。
    pub screen: Screen,
}

impl DeviceConfig {
    /// 从 TOML 文本解析。
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// 读取文件。
    pub fn load(path: &str) -> anyhow::Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// 没有设备文件时的默认值（开发用）。
    pub fn fallback(width: u32, height: u32, scale: f64) -> Self {
        DeviceConfig {
            name: "开发机".into(),
            size_class: SizeClass::Regular,
            screen: Screen {
                width,
                height,
                scale: Some(scale),
                diagonal_inch: None,
                refresh_hz: 60,
                corner_radius: 0.0,
            },
        }
    }

    /// 实际使用的缩放。目标是平板上 1 逻辑点 ≈ 1/132 英寸（与常见平板界面密度接近）。
    pub fn scale(&self) -> f64 {
        if let Some(s) = self.screen.scale {
            return s;
        }
        if let Some(diag) = self.screen.diagonal_inch {
            let (w, h) = (self.screen.width as f64, self.screen.height as f64);
            let ppi = (w * w + h * h).sqrt() / diag;
            // 四舍五入到 0.25 的倍数，避免奇怪的小数缩放。
            return ((ppi / 132.0) * 4.0).round().max(4.0) / 4.0;
        }
        1.0
    }

    /// 逻辑尺寸（点）。
    pub fn logical_size(&self) -> (f64, f64) {
        let s = self.scale();
        (self.screen.width as f64 / s, self.screen.height as f64 / s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_derives_scale() {
        let d = DeviceConfig::parse(
            r#"
            name = "小米平板 5"
            [screen]
            width = 2560
            height = 1600
            diagonal_inch = 11.0
            refresh_hz = 120
            corner_radius = 18
            "#,
        )
        .unwrap();
        assert_eq!(d.size_class, SizeClass::Regular);
        assert_eq!(d.scale(), 2.0);
        assert_eq!(d.logical_size(), (1280.0, 800.0));
        assert_eq!(d.screen.refresh_hz, 120);
    }
}
