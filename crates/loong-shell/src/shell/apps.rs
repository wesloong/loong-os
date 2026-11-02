//! 应用列表：内置的系统应用与演示应用，加上 PWA 注册表（`~/.local/share/loong/webapps/`，
//! 见 [`crate::registry`]）中已安装的 Web 应用（[`web_app`]）。

use loong_ui::ink::{fallback_pigment, pigment_for_color, suggested_pigment};

use crate::registry::WebApp;

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
    /// 已安装的 Web 应用（来自注册表）的 manifest；内置应用为 `None`。只有它们可以从主屏移除。
    pub web: Option<WebApp>,
    /// 已从主屏移除（下标在合成器运行期间保持不变，见 [`super::webapps`]）。
    pub removed: bool,
}

fn app(id: &str, name: &str, symbol: &str, in_dock: bool) -> AppEntry {
    AppEntry {
        id: id.into(),
        name: name.into(),
        symbol: symbol.into(),
        pigment: suggested_pigment(symbol),
        command: None,
        in_dock,
        web: None,
        removed: false,
    }
}

/// 已安装的 Web 应用：主屏标签用 `short_name`；颜料按 `theme_color`（没有或太灰时按
/// `background_color`，再没有就按 id）选一种传统颜料；`command` 为 loong-webapp 启动命令。
pub fn web_app(app: &WebApp, command: String) -> AppEntry {
    AppEntry {
        id: app.id.clone(),
        name: app.short_name.clone(),
        symbol: String::new(),
        pigment: web_pigment(app),
        command: Some(command),
        in_dock: false,
        web: Some(app.clone()),
        removed: false,
    }
}

/// Web 应用的颜料（见 [`web_app`]）。
pub fn web_pigment(app: &WebApp) -> u32 {
    app.theme_color
        .and_then(pigment_for_color)
        .or_else(|| app.background_color.and_then(pigment_for_color))
        .unwrap_or_else(|| fallback_pigment(id_seed(&app.id)))
}

/// 由应用 id 得到的稳定种子（FNV-1a）：图标纸纹、颜料轮换，与应用在列表中的位置无关。
pub fn id_seed(id: &str) -> u32 {
    id.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    })
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

/// loong-webapp 运行应用 `id` 时窗口的 app_id：`loong.webapp.<id>`，id 中的 `-` 写作 `_`，
/// 以数字开头的 id 前面再加 `_`（GApplication id 的每一节不能以数字开头，WebKit 用它作 Wayland
/// app_id）。规则与 loong-webapp 相同；应用 id 不以 `-` 开头，所以没有歧义。
pub fn webapp_app_id(id: &str) -> String {
    let escape = if id.starts_with(|c: char| c.is_ascii_digit()) {
        "_"
    } else {
        ""
    };
    format!("loong.webapp.{escape}{}", id.replace('-', "_"))
}

/// 窗口的 Wayland app_id 对应哪个应用：
/// - `loong.webapp.<id>`：loong-webapp 运行的 Web 应用（见 [`webapp_app_id`]）；
/// - `loong.<id>`：系统组件，例如系统浏览器 `loong.browser`；
/// - 其余按应用 id 原样匹配。
pub fn app_for_app_id(apps: &[AppEntry], app_id: &str) -> Option<usize> {
    let id = app_id
        .strip_prefix("loong.webapp.")
        .map(|id| id.strip_prefix('_').unwrap_or(id).replace('_', "-"))
        .or_else(|| app_id.strip_prefix("loong.").map(str::to_string))
        .unwrap_or_else(|| app_id.to_string());
    apps.iter().position(|a| a.id == id && !a.removed)
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
        // 已安装的 Web 应用：以数字开头的 id 在 app_id 里前面多一个 '_'；已移除的不再匹配。
        let mut apps = apps;
        for id in ["127-0-0-1-8765-pwa", "example-com"] {
            let manifest = format!(r#"{{"name": "x", "start_url": "https://{id}.example/"}}"#);
            let app = crate::registry::parse_manifest(id, &manifest).unwrap();
            apps.push(web_app(&app, String::new()));
            let i = apps.len() - 1;
            assert_eq!(app_for_app_id(&apps, &webapp_app_id(id)), Some(i), "{id}");
        }
        assert_eq!(
            webapp_app_id("127-0-0-1-8765-pwa"),
            "loong.webapp._127_0_0_1_8765_pwa"
        );
        assert_eq!(webapp_app_id("example-com"), "loong.webapp.example_com");
        let last = apps.len() - 1;
        apps[last].removed = true;
        assert_eq!(app_for_app_id(&apps, "loong.webapp.example_com"), None);
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
