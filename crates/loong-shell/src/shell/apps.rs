//! 应用列表。v1 先内置一组演示应用；M2 起由 PWA 注册表（`~/.local/share/loong/webapps/`）提供。

/// 一个应用。
#[derive(Debug, Clone)]
pub struct AppEntry {
    /// 唯一标识。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 图标符号（Lucide 名称）。
    pub symbol: String,
    /// 图标渐变（上、下），0xRRGGBB。
    pub colors: (u32, u32),
    /// 启动命令；为空时显示占位内容（用于演示和测试动画）。
    pub command: Option<String>,
    /// 是否放在 Dock。
    pub in_dock: bool,
}

fn app(id: &str, name: &str, symbol: &str, colors: (u32, u32), in_dock: bool) -> AppEntry {
    AppEntry {
        id: id.into(),
        name: name.into(),
        symbol: symbol.into(),
        colors,
        command: None,
        in_dock,
    }
}

/// 内置演示应用。
pub fn builtin_apps() -> Vec<AppEntry> {
    vec![
        app("browser", "浏览器", "compass", (0x2DD4BF, 0x0F766E), true),
        app(
            "messages",
            "信息",
            "message-circle",
            (0x4ADE80, 0x15803D),
            true,
        ),
        app("mail", "邮件", "mail", (0x60A5FA, 0x1D4ED8), true),
        app("music", "音乐", "music", (0xFB7185, 0xBE123C), true),
        app("settings", "设置", "settings", (0x9CA3AF, 0x4B5563), true),
        app("photos", "相册", "image", (0xFDBA74, 0xEA580C), false),
        app(
            "notes",
            "备忘录",
            "notebook-pen",
            (0xFDE68A, 0xD97706),
            false,
        ),
        app("calendar", "日历", "calendar", (0xF87171, 0xB91C1C), false),
        app("clock", "时钟", "clock", (0x475569, 0x0F172A), false),
        app("files", "文件", "folder", (0x7DD3FC, 0x0369A1), false),
        app("camera", "相机", "camera", (0x6B7280, 0x1F2937), false),
        app("maps", "地图", "map", (0x86EFAC, 0x16A34A), false),
        app(
            "calculator",
            "计算器",
            "calculator",
            (0xFDBA74, 0xC2410C),
            false,
        ),
        app("weather", "天气", "cloud-sun", (0x93C5FD, 0x2563EB), false),
        app("videos", "视频", "play-circle", (0xC4B5FD, 0x6D28D9), false),
        app("books", "图书", "book-open", (0xFCD34D, 0xB45309), false),
        app("recorder", "录音机", "mic", (0x94A3B8, 0x334155), false),
        app(
            "reminders",
            "提醒事项",
            "list-checks",
            (0xA5B4FC, 0x4338CA),
            false,
        ),
        app("contacts", "通讯录", "contact", (0xD6D3D1, 0x78716C), false),
        app("health", "健康", "heart-pulse", (0xFDA4AF, 0xE11D48), false),
        app("stocks", "股票", "trending-up", (0x4B5563, 0x111827), false),
        app(
            "translate",
            "翻译",
            "languages",
            (0x67E8F9, 0x0E7490),
            false,
        ),
        app("podcasts", "播客", "podcast", (0xD8B4FE, 0x7E22CE), false),
        app("home", "家庭", "house", (0xFCD34D, 0xEA580C), false),
        app("wallet", "钱包", "wallet", (0x374151, 0x030712), false),
        app("store", "商店", "shopping-bag", (0x7DD3FC, 0x2563EB), false),
        app("games", "游戏", "gamepad-2", (0xF0ABFC, 0xA21CAF), false),
        app("news", "新闻", "newspaper", (0xFCA5A5, 0xDC2626), false),
        app("timer", "计时器", "timer", (0xFDE047, 0xCA8A04), false),
        // 以下应用排在主屏第二页（横屏每页 6 × 4 个格子）。
        app("measure", "测距仪", "ruler", (0xA3A3A3, 0x404040), false),
        app("scanner", "扫描", "scan-line", (0x6EE7B7, 0x047857), false),
        app("terminal", "终端", "terminal", (0x52525B, 0x18181B), false),
    ]
}
