//! 应用列表。v1 先内置一组演示应用；M2 起由 PWA 注册表（`~/.local/share/loong/webapps/`）提供。

use loong_ui::ink::suggested_pigment;

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
    /// 应用画面的色调。每个应用只用一种颜料；内置应用按图标符号取
    /// [`suggested_pigment`]，与 loong-ui、LoongKit 的选色一致（之后 PWA 可在清单中指定）。
    pub pigment: u32,
    /// 启动命令；为空时显示占位内容（用于演示和测试动画）。
    pub command: Option<String>,
    /// 是否放在 Dock。
    pub in_dock: bool,
}

fn app(id: &str, name: &str, symbol: &str, in_dock: bool) -> AppEntry {
    AppEntry {
        id: id.into(),
        name: name.into(),
        symbol: symbol.into(),
        pigment: suggested_pigment(symbol),
        command: None,
        in_dock,
    }
}

/// 内置演示应用。
///
/// 颜料按“画意”分配（见 [`suggested_pigment`]）：天光水色用石青/花青，草木山野用石绿，
/// 日色暖物用藤黄/赭石，情感与艺术用胭脂；朱砂留给品牌与提示，不给应用。
pub fn builtin_apps() -> Vec<AppEntry> {
    vec![
        app("browser", "浏览器", "compass", true),
        app("messages", "信息", "message-circle", true),
        app("mail", "邮件", "mail", true),
        app("music", "音乐", "music", true),
        app("settings", "设置", "settings", true),
        app("photos", "相册", "image", false),
        app("notes", "备忘录", "notebook-pen", false),
        app("calendar", "日历", "calendar", false),
        app("clock", "时钟", "clock", false),
        app("files", "文件", "folder", false),
        app("camera", "相机", "camera", false),
        app("maps", "地图", "map", false),
        app("calculator", "计算器", "calculator", false),
        app("weather", "天气", "cloud-sun", false),
        app("videos", "视频", "play-circle", false),
        app("books", "图书", "book-open", false),
        app("recorder", "录音机", "mic", false),
        app("reminders", "提醒事项", "list-checks", false),
        app("contacts", "通讯录", "contact", false),
        app("health", "健康", "heart-pulse", false),
        app("stocks", "股票", "trending-up", false),
        app("translate", "翻译", "languages", false),
        app("podcasts", "播客", "podcast", false),
        app("home", "家庭", "house", false),
        app("wallet", "钱包", "wallet", false),
        app("store", "商店", "shopping-bag", false),
        app("games", "游戏", "gamepad-2", false),
        app("news", "新闻", "newspaper", false),
        app("timer", "计时器", "timer", false),
        // 以下应用排在主屏第二页（横屏每页 6 × 4 个格子）。
        app("measure", "测距仪", "ruler", false),
        app("scanner", "扫描", "scan-line", false),
        app("terminal", "终端", "terminal", false),
    ]
}

/// 窗口的 Wayland app_id 对应哪个应用：
/// - `loong.webapp.<id>`：loong-webapp 运行的 Web 应用（id 中的 `-` 在 app_id 里写作 `_`）；
/// - `loong.<id>`：系统组件，例如系统浏览器 `loong.browser`；
/// - 其余按应用 id 原样匹配。
pub fn app_for_app_id(apps: &[AppEntry], app_id: &str) -> Option<usize> {
    let id = app_id
        .strip_prefix("loong.webapp.")
        .map(|id| id.replace('_', "-"))
        .or_else(|| app_id.strip_prefix("loong.").map(str::to_string))
        .unwrap_or_else(|| app_id.to_string());
    apps.iter().position(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_map_to_apps() {
        let apps = builtin_apps();
        let index = |id: &str| apps.iter().position(|a| a.id == id);
        assert_eq!(app_for_app_id(&apps, "loong.browser"), index("browser"));
        assert_eq!(
            app_for_app_id(&apps, "loong.webapp.settings"),
            index("settings")
        );
        assert_eq!(app_for_app_id(&apps, "music"), index("music"));
        assert!(index("settings") != index("browser"));
        assert_eq!(app_for_app_id(&apps, "loong.webapp.unknown_app"), None);
        assert_eq!(app_for_app_id(&apps, "foot"), None);
    }

    #[test]
    fn every_app_has_a_traditional_pigment() {
        use loong_ui::theme::pigment::*;
        let apps = builtin_apps();
        let allowed = [
            ZHUSHA, YANZHI, TENGHUANG, ZHESHI, SHIQING, SHILV, HUAQING, NIJIN,
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
        // 朱砂只用于品牌与重要提示，不给应用。
        assert!(apps.iter().all(|a| a.pigment != ZHUSHA));
        // 天气藤黄、地图石绿、相册石青、音乐胭脂。
        let of = |id: &str| apps.iter().find(|a| a.id == id).unwrap().pigment;
        assert_eq!(
            [of("weather"), of("maps"), of("photos"), of("music")],
            [TENGHUANG, SHILV, SHIQING, YANZHI]
        );
    }
}
