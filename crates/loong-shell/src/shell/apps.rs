//! 应用列表。v1 先内置一组演示应用；M2 起由 PWA 注册表（`~/.local/share/loong/webapps/`）提供。

use loong_ui::theme::pigment::{HUAQING, NIJIN, SHILV, SHIQING, TENGHUANG, YANZHI, ZHESHI, ZHUSHA};

/// 墨（用作颜料时取“重墨”的暖灰）：工具类应用的点缀，与五色墨同源。
const MO: u32 = 0x5C574E;

/// 一个应用。
#[derive(Debug, Clone)]
pub struct AppEntry {
    /// 唯一标识。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 图标符号（Lucide 名称，由 loong-ui 转成毛笔笔触）。
    pub symbol: String,
    /// 点缀颜料（0xRRGGBB，中国传统颜料，见 [`loong_ui::theme::pigment`]）：图标的淡彩色晕、
    /// 应用画面的色调。每个应用只用一种颜料。
    pub pigment: u32,
    /// 启动命令；为空时显示占位内容（用于演示和测试动画）。
    pub command: Option<String>,
    /// 是否放在 Dock。
    pub in_dock: bool,
}

fn app(id: &str, name: &str, symbol: &str, pigment: u32, in_dock: bool) -> AppEntry {
    AppEntry {
        id: id.into(),
        name: name.into(),
        symbol: symbol.into(),
        pigment,
        command: None,
        in_dock,
    }
}

/// 内置演示应用。
///
/// 颜料按“画意”分配：天光水色用石青/花青，草木山野用石绿，日色暖物用藤黄/赭石，
/// 情感与艺术用胭脂，日历/新闻等需要醒目的用朱砂，工具类用墨；同一行相邻的应用尽量不同色，
/// 让主屏像一幅设色清淡、疏密有致的画。
pub fn builtin_apps() -> Vec<AppEntry> {
    vec![
        app("browser", "浏览器", "compass", HUAQING, true),
        app("messages", "信息", "message-circle", SHILV, true),
        app("mail", "邮件", "mail", SHIQING, true),
        app("music", "音乐", "music", YANZHI, true),
        app("settings", "设置", "settings", MO, true),
        app("photos", "相册", "image", SHIQING, false),
        app("notes", "备忘录", "notebook-pen", TENGHUANG, false),
        app("calendar", "日历", "calendar", ZHUSHA, false),
        app("clock", "时钟", "clock", MO, false),
        app("files", "文件", "folder", HUAQING, false),
        app("camera", "相机", "camera", ZHESHI, false),
        app("maps", "地图", "map", SHILV, false),
        app("calculator", "计算器", "calculator", ZHESHI, false),
        app("weather", "天气", "cloud-sun", TENGHUANG, false),
        app("videos", "视频", "play-circle", YANZHI, false),
        app("books", "图书", "book-open", ZHESHI, false),
        app("recorder", "录音机", "mic", MO, false),
        app("reminders", "提醒事项", "list-checks", HUAQING, false),
        app("contacts", "通讯录", "contact", ZHESHI, false),
        app("health", "健康", "heart-pulse", YANZHI, false),
        app("stocks", "股票", "trending-up", SHILV, false),
        app("translate", "翻译", "languages", SHIQING, false),
        app("podcasts", "播客", "podcast", NIJIN, false),
        app("home", "家庭", "house", ZHESHI, false),
        app("wallet", "钱包", "wallet", NIJIN, false),
        app("store", "商店", "shopping-bag", HUAQING, false),
        app("games", "游戏", "gamepad-2", YANZHI, false),
        app("news", "新闻", "newspaper", ZHUSHA, false),
        app("timer", "计时器", "timer", TENGHUANG, false),
        // 以下应用排在主屏第二页（横屏每页 6 × 4 个格子）。
        app("measure", "测距仪", "ruler", ZHESHI, false),
        app("scanner", "扫描", "scan-line", SHILV, false),
        app("terminal", "终端", "terminal", MO, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_a_traditional_pigment() {
        let apps = builtin_apps();
        let allowed = [
            ZHUSHA, YANZHI, TENGHUANG, ZHESHI, SHIQING, SHILV, HUAQING, NIJIN, MO,
        ];
        for a in &apps {
            assert!(allowed.contains(&a.pigment), "{}", a.id);
            assert!(
                loong_ui::icon::builtin_symbol(&a.symbol).is_some(),
                "{}",
                a.symbol
            );
        }
        // 用得最多的颜料不超过四分之一：既有变化，又不杂乱。
        for p in allowed {
            let n = apps.iter().filter(|a| a.pigment == p).count();
            assert!(n * 4 <= apps.len(), "{p:06X} 用了 {n} 次");
        }
        // 朱砂只给少数需要醒目的应用（品牌与重要提示色）。
        assert!(apps.iter().filter(|a| a.pigment == ZHUSHA).count() <= 2);
    }
}
