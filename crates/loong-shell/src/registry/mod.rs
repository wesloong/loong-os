//! PWA 注册表：浏览器“添加到主屏幕”写入的 `$XDG_DATA_HOME/loong/webapps/<id>/manifest.json`。
//!
//! 格式与约定见 `docs/api/loong-webapps.md`。要点：
//!
//! - 每个子目录是一个应用，目录名即应用 id（只允许 `[a-z0-9-]`，最长 64，与 loong-webapp 相同）；
//!   目录里有 `manifest.json` 才算已安装。同一目录也是 loong-webapp 给这个应用用的 WebKit 数据目录，
//!   所以只认 `manifest.json` 与图标文件，其余文件一概不看。
//! - `manifest.json` 是浏览器规范化过的 W3C manifest（绝对地址、`name`/`short_name`/`start_url`/
//!   `scope`/`display`/颜色/`icons`），外加 Loong 自己的 `loong_icon`：浏览器下载好的图标文件
//!   （`icon.png` 或 `icon.svg`）及其用途（`any` / `maskable`）。
//! - 浏览器先写图标、最后用 rename 原子地写 `manifest.json`，所以读到 manifest 时图标已经就位。
//!
//! 本模块只做读取、校验、比较与删除（纯函数，便于测试）；目录监视见 [`watch`]，主屏排列的
//! 持久化见 [`order`]，与合成器事件循环的衔接见 [`service`]。

pub mod order;
pub mod service;
pub mod watch;

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use loong_ui::ink::IconFormat;
use serde_json::Value;

/// manifest.json 的大小上限（浏览器写入时的上限相同）。
pub const MAX_MANIFEST: u64 = 64 * 1024;
/// 图标文件的大小上限。
pub const MAX_ICON_FILE: u64 = 1024 * 1024;
/// 应用名称（主屏标签用 `short_name`）的长度上限（字符）。
const MAX_NAME: usize = 80;
/// 网址的长度上限。
const MAX_URL: usize = 2048;

/// 注册表中的一个应用。
#[derive(Debug, Clone, PartialEq)]
pub struct WebApp {
    /// 应用 id（= 目录名，`[a-z0-9-]`）。
    pub id: String,
    /// 完整名称（多任务、移除确认等处）。
    pub name: String,
    /// 短名称（主屏图标下的标签）。
    pub short_name: String,
    /// 起始地址（http/https）。
    pub start_url: String,
    /// 作用域（地址前缀，起始地址必在其中）。
    pub scope: String,
    /// manifest 的 `display`（v1 一律按 standalone 全屏运行，这里只记录）。
    pub display: String,
    /// 主题色 0xRRGGBB：状态栏的初始墨色、没有图标时首字小笺的颜料。
    pub theme_color: Option<u32>,
    /// 背景色 0xRRGGBB：启动闪屏（应用卡片）与 WebKit 首帧前的底色。
    pub background_color: Option<u32>,
    /// 浏览器下载好的图标（没有时主屏用名称首字）。
    pub icon: Option<Icon>,
    /// manifest.json 的修改时间（Unix 秒）：启动时给 home.toml 里没有的应用排序。
    pub installed: u64,
}

/// 图标文件的内容。
#[derive(Debug, Clone, PartialEq)]
pub struct Icon {
    pub data: Arc<[u8]>,
    pub format: IconFormat,
    /// `purpose` 含 `maskable`：图标铺满小笺。
    pub maskable: bool,
}

/// 注册表的变化（由前后两次扫描比较得出）。
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Added(WebApp),
    /// manifest 或图标变了（重新安装、网站更新了图标）。
    Updated(WebApp),
    Removed(String),
}

/// 应用 id 是否合法：`[a-z0-9-]`，1–64 个字符，不以 `-` 开头（与 loong-webapp、浏览器的规则一致）。
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn non_empty_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_DATA_HOME`（默认 `~/.local/share`）。
pub fn data_home() -> Option<PathBuf> {
    non_empty_env("XDG_DATA_HOME").or_else(|| non_empty_env("HOME").map(|h| h.join(".local/share")))
}

/// `$XDG_CACHE_HOME`（默认 `~/.cache`）。
pub fn cache_home() -> Option<PathBuf> {
    non_empty_env("XDG_CACHE_HOME").or_else(|| non_empty_env("HOME").map(|h| h.join(".cache")))
}

/// 注册表目录：`$XDG_DATA_HOME/loong/webapps`。
pub fn registry_dir() -> Option<PathBuf> {
    Some(data_home()?.join("loong").join("webapps"))
}

/// 应用的缓存目录（loong-webapp 的 WebKit 缓存）：`$XDG_CACHE_HOME/loong/webapps/<id>`。
pub fn cache_dir(id: &str) -> Option<PathBuf> {
    Some(cache_home()?.join("loong").join("webapps").join(id))
}

/// 解析 CSS 颜色（manifest 的 `theme_color` / `background_color`）→ 0xRRGGBB，忽略透明度。
/// 支持 `#rgb`、`#rgba`、`#rrggbb`、`#rrggbbaa`、`rgb()`/`rgba()`（数字或百分比）和常见的颜色名；
/// 浏览器安装时已经规范化成 `#rrggbb`，其余写法用于手写的 manifest。
pub fn parse_color(text: &str) -> Option<u32> {
    let s = text.trim().to_ascii_lowercase();
    if let Some(hex) = s.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
        return match digits.len() {
            3 | 4 => Some(digits[..3].iter().fold(0, |acc, &d| (acc << 8) | (d * 17))),
            6 | 8 => Some(digits[..6].iter().fold(0, |acc, &d| (acc << 4) | d)),
            _ => None,
        };
    }
    if let Some(args) = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts: Vec<&str> = args
            .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        if !(3..=4).contains(&parts.len()) {
            return None;
        }
        let channel = |p: &str| -> Option<u32> {
            let v = match p.strip_suffix('%') {
                Some(pct) => pct.parse::<f64>().ok()? * 255.0 / 100.0,
                None => p.parse::<f64>().ok()?,
            };
            v.is_finite().then(|| v.round().clamp(0.0, 255.0) as u32)
        };
        let (r, g, b) = (channel(parts[0])?, channel(parts[1])?, channel(parts[2])?);
        return Some(r << 16 | g << 8 | b);
    }
    Some(match s.as_str() {
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        "red" => 0xFF0000,
        "green" => 0x008000,
        "blue" => 0x0000FF,
        "yellow" => 0xFFFF00,
        "orange" => 0xFFA500,
        "purple" => 0x800080,
        "teal" => 0x008080,
        "navy" => 0x000080,
        "maroon" => 0x800000,
        "olive" => 0x808000,
        "gray" | "grey" => 0x808080,
        "silver" => 0xC0C0C0,
        "brown" => 0xA52A2A,
        "pink" => 0xFFC0CB,
        "crimson" => 0xDC143C,
        "indigo" => 0x4B0082,
        "darkgreen" => 0x006400,
        "steelblue" => 0x4682B4,
        "whitesmoke" => 0xF5F5F5,
        _ => return None,
    })
}

/// 去掉控制字符、首尾空白，截到 `max` 个字符。
fn clean_text(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// 可用作应用地址的 http(s) 网址（不含空白与控制字符，长度有上限）。
fn web_url(s: &str) -> Option<String> {
    let ok = (s.starts_with("https://") || s.starts_with("http://"))
        && s.len() <= MAX_URL
        && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then(|| s.to_string())
}

/// 网址所在“目录”：去掉查询与片段后，截到最后一个 `/`（`https://a.b/x/y?q` → `https://a.b/x/`）。
fn url_dir(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let base = &url[..end];
    // 跳过 scheme://host 部分，只在路径里找 '/'。
    let path_start = base
        .find("://")
        .and_then(|i| base[i + 3..].find('/').map(|j| i + 3 + j));
    match path_start {
        Some(p) => base[..=base[p..].rfind('/').map_or(p, |j| p + j)].to_string(),
        None => format!("{base}/"),
    }
}

/// manifest 里的 `loong_icon`（浏览器写入）：`{ "file": "icon.png", "purpose": "maskable" }`。
/// 文件名只允许 `icon.png` / `icon.svg`（不跟随任意路径）。
fn icon_ref(m: &Value) -> Option<(&'static str, bool)> {
    let icon = m.get("loong_icon")?;
    let file = match icon.get("file")?.as_str()? {
        "icon.png" => "icon.png",
        "icon.svg" => "icon.svg",
        _ => return None,
    };
    let maskable = icon
        .get("purpose")
        .and_then(Value::as_str)
        .is_some_and(|p| p.split_ascii_whitespace().any(|w| w == "maskable"));
    Some((file, maskable))
}

/// 读取并识别图标文件（大小有上限，内容须是 PNG 或 SVG）。
pub fn read_icon(path: &Path, maskable: bool) -> Option<Icon> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ICON_FILE {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let format = loong_ui::ink::sniff_icon(&data)?;
    Some(Icon {
        data: data.into(),
        format,
        maskable,
    })
}

/// 解析一个应用的 manifest（`id` 为目录名）。图标由 [`load`] 另外读取。
pub fn parse_manifest(id: &str, text: &str) -> Result<WebApp, String> {
    if !valid_id(id) {
        return Err(format!("应用 id 不合法：{id:?}"));
    }
    let m: Value =
        serde_json::from_str(text).map_err(|e| format!("manifest 不是合法的 JSON：{e}"))?;
    if !m.is_object() {
        return Err("manifest 不是 JSON 对象".into());
    }
    let text_of = |key: &str| {
        m.get(key)
            .and_then(Value::as_str)
            .map(|s| clean_text(s, MAX_NAME))
            .filter(|s| !s.is_empty())
    };
    let name = text_of("name")
        .or_else(|| text_of("short_name"))
        .ok_or("manifest 没有 name 或 short_name")?;
    let short_name = text_of("short_name").unwrap_or_else(|| name.clone());
    let start_url = m
        .get("start_url")
        .and_then(Value::as_str)
        .and_then(web_url)
        .ok_or("start_url 不是 http(s) 网址")?;
    // 作用域：必须包含起始地址；缺省或不合规时取起始地址所在目录（与浏览器的规范化一致）。
    let scope = m
        .get("scope")
        .and_then(Value::as_str)
        .and_then(web_url)
        .filter(|s| start_url.starts_with(s.as_str()))
        .unwrap_or_else(|| url_dir(&start_url));
    let display = m
        .get("display")
        .and_then(Value::as_str)
        .filter(|d| ["fullscreen", "standalone", "minimal-ui", "browser"].contains(d))
        .unwrap_or("standalone")
        .to_string();
    let color = |key: &str| m.get(key).and_then(Value::as_str).and_then(parse_color);
    Ok(WebApp {
        id: id.to_string(),
        name,
        short_name,
        start_url,
        scope,
        display,
        theme_color: color("theme_color"),
        background_color: color("background_color"),
        icon: None,
        installed: 0,
    })
}

/// 读取 `dir`（某个应用的目录）：manifest + 图标。不是已安装的应用（没有 manifest）时为 `Ok(None)`。
pub fn load(dir: &Path) -> Result<Option<WebApp>, String> {
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("目录名不是 UTF-8")?;
    let path = dir.join("manifest.json");
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if meta.len() > MAX_MANIFEST {
        return Err(format!("{} 太大（{} 字节）", path.display(), meta.len()));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut app = parse_manifest(id, &text)?;
    app.installed = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let manifest: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if let Some((file, maskable)) = icon_ref(&manifest) {
        app.icon = read_icon(&dir.join(file), maskable);
        if app.icon.is_none() {
            tracing::warn!("{id}：图标 {file} 读不出来，主屏用名称首字");
        }
    }
    Ok(Some(app))
}

/// 扫描注册表目录，返回全部已安装的应用（按 id 排序）。`reserved` 中的 id（内置应用）被忽略。
/// 不合法的应用只记录警告、不影响其他应用。
pub fn scan(root: &Path, reserved: &[String]) -> Vec<WebApp> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut apps: Vec<WebApp> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name();
            let id = name.to_str()?;
            if !valid_id(id) {
                return None;
            }
            if reserved.iter().any(|r| r == id) {
                // 系统应用（设置等）也在这里放 WebKit 数据，但不是注册表里的应用。
                return None;
            }
            match load(&e.path()) {
                Ok(app) => app,
                Err(err) => {
                    tracing::warn!("忽略注册表中的 {id}：{err}");
                    None
                }
            }
        })
        .collect();
    apps.sort_by(|a, b| a.id.cmp(&b.id));
    apps
}

/// 比较两次扫描（都按 id 排序）：新增、变化、移除。
pub fn diff(old: &[WebApp], new: &[WebApp]) -> Vec<Change> {
    let mut out = Vec::new();
    for app in new {
        match old.iter().find(|o| o.id == app.id) {
            None => out.push(Change::Added(app.clone())),
            // 只是重写了 manifest（修改时间变了）不算变化。
            Some(o) if !same_app(o, app) => out.push(Change::Updated(app.clone())),
            Some(_) => {}
        }
    }
    for o in old {
        if !new.iter().any(|a| a.id == o.id) {
            out.push(Change::Removed(o.id.clone()));
        }
    }
    out
}

fn same_app(a: &WebApp, b: &WebApp) -> bool {
    WebApp {
        installed: 0,
        ..a.clone()
    } == WebApp {
        installed: 0,
        ..b.clone()
    }
}

/// 给 `/bin/sh -c` 用的单引号转义。
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 启动命令：`loong-webapp --id <id> --name <名称> --scope <作用域> [--theme-color #rrggbb]
/// [--background-color #rrggbb] <起始地址>`。`webapp` 为 loong-webapp 的命令（环境变量
/// `LOONG_WEBAPP`，默认 `loong-webapp`；开发时可以是带环境变量的完整命令，不加引号）。
pub fn launch_command(app: &WebApp, webapp: &str) -> String {
    let mut cmd = format!(
        "{webapp} --id {} --name {} --scope {}",
        shell_quote(&app.id),
        shell_quote(&app.name),
        shell_quote(&app.scope)
    );
    if let Some(c) = app.theme_color {
        cmd += &format!(" --theme-color '#{c:06x}'");
    }
    if let Some(c) = app.background_color {
        cmd += &format!(" --background-color '#{c:06x}'");
    }
    cmd += " ";
    cmd += &shell_quote(&app.start_url);
    cmd
}

/// loong-webapp 的命令（`LOONG_WEBAPP`，默认 `loong-webapp`）。
pub fn webapp_command() -> String {
    std::env::var("LOONG_WEBAPP")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "loong-webapp".into())
}

/// 卸载：先删 manifest（注册表立即认为它已移除），再删整个应用目录（含 loong-webapp 的 WebKit
/// 数据）与缓存目录。目录不存在不算错误。
pub fn uninstall(root: &Path, id: &str) -> io::Result<()> {
    if !valid_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "应用 id 不合法",
        ));
    }
    let dir = root.join(id);
    let ignore_missing = |r: io::Result<()>| match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    };
    ignore_missing(std::fs::remove_file(dir.join("manifest.json")))?;
    ignore_missing(std::fs::remove_dir_all(&dir))?;
    if let Some(cache) = cache_dir(id) {
        ignore_missing(std::fs::remove_dir_all(cache))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r##"{
      "name": "山居笔记",
      "short_name": "山居",
      "start_url": "http://127.0.0.1:8765/app/index.html?source=pwa",
      "scope": "http://127.0.0.1:8765/app/",
      "display": "standalone",
      "theme_color": "#2e5e74",
      "background_color": "#F3EEE2",
      "icons": [{ "src": "http://127.0.0.1:8765/app/icon-192.png", "sizes": "192x192" }],
      "loong_icon": { "file": "icon.png", "purpose": "any maskable", "src": "http://127.0.0.1:8765/app/icon-192.png" }
    }"##;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loong-registry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png() -> Vec<u8> {
        let mut pm = loong_ui::pixmap(4, 4);
        pm.fill(loong_ui::tiny_skia::Color::from_rgba8(46, 94, 116, 255));
        pm.encode_png().unwrap()
    }

    #[test]
    fn parses_a_normalized_manifest() {
        let app = parse_manifest("127-0-0-1-8765-app", MANIFEST).unwrap();
        assert_eq!(app.name, "山居笔记");
        assert_eq!(app.short_name, "山居");
        assert_eq!(app.scope, "http://127.0.0.1:8765/app/");
        assert_eq!(app.theme_color, Some(0x2E5E74));
        assert_eq!(app.background_color, Some(0xF3EEE2));
        assert_eq!(app.display, "standalone");
        assert_eq!(
            icon_ref(&serde_json::from_str(MANIFEST).unwrap()),
            Some(("icon.png", true))
        );
    }

    #[test]
    fn rejects_and_repairs_bad_manifests() {
        let p = |text: &str| parse_manifest("app", text);
        assert!(p("[]").is_err());
        assert!(p("not json").is_err());
        assert!(
            p(r#"{"start_url": "https://a.example/"}"#).is_err(),
            "没有名称"
        );
        assert!(p(r#"{"name": "x", "start_url": "javascript:alert(1)"}"#).is_err());
        assert!(p(r#"{"name": "x", "start_url": "/relative"}"#).is_err());
        assert!(p(r#"{"name": "x", "start_url": "https://a.example/ b"}"#).is_err());
        assert!(parse_manifest("Bad_Id", MANIFEST).is_err());
        assert!(parse_manifest("../etc", MANIFEST).is_err());
        assert!(parse_manifest("-x", MANIFEST).is_err());
        // 只有 short_name、作用域不含起始地址、display 不认识、颜色写法各异。
        let app = p(
            r#"{"short_name": " 墨\u0007 ", "start_url": "https://a.example/x/y?q=1",
                        "scope": "https://b.example/", "display": "window",
                        "theme_color": "rgb(46, 94, 116)", "background_color": "nonsense"}"#,
        )
        .unwrap();
        assert_eq!((app.name.as_str(), app.short_name.as_str()), ("墨", "墨"));
        assert_eq!(app.scope, "https://a.example/x/");
        assert_eq!(app.display, "standalone");
        assert_eq!(app.theme_color, Some(0x2E5E74));
        assert_eq!(app.background_color, None);
        // 名称截断。
        let long = format!(
            r#"{{"name": "{}", "start_url": "https://a.example/"}}"#,
            "长".repeat(200)
        );
        assert_eq!(p(&long).unwrap().name.chars().count(), MAX_NAME);
        // loong_icon 只认固定文件名。
        let evil: Value =
            serde_json::from_str(r#"{"loong_icon": {"file": "../../.ssh/id_rsa"}}"#).unwrap();
        assert_eq!(icon_ref(&evil), None);
    }

    #[test]
    fn parses_css_colors() {
        assert_eq!(parse_color("#fff"), Some(0xFFFFFF));
        assert_eq!(parse_color("#1A73E8"), Some(0x1A73E8));
        assert_eq!(parse_color("#1a73e880"), Some(0x1A73E8));
        assert_eq!(parse_color("#abcd"), Some(0xAABBCC));
        assert_eq!(parse_color(" RGBA(255, 0, 0, 0.5) "), Some(0xFF0000));
        assert_eq!(parse_color("rgb(100% 50% 0%)"), Some(0xFF8000));
        assert_eq!(parse_color("white"), Some(0xFFFFFF));
        for bad in [
            "",
            "#12",
            "#ggg",
            "rgb(1,2)",
            "hsl(0 0% 0%)",
            "transparentish",
        ] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn url_dirs() {
        assert_eq!(url_dir("https://a.b/x/y?q#f"), "https://a.b/x/");
        assert_eq!(url_dir("https://a.b"), "https://a.b/");
        assert_eq!(url_dir("https://a.b/"), "https://a.b/");
        assert_eq!(url_dir("http://h:8765/index.html"), "http://h:8765/");
    }

    #[test]
    fn scan_diff_and_uninstall() {
        let root = temp_dir("scan");
        let install = |id: &str, text: &str, icon: Option<(&str, &[u8])>| {
            let dir = root.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some((name, data)) = icon {
                std::fs::write(dir.join(name), data).unwrap();
            }
            std::fs::write(dir.join("manifest.json"), text).unwrap();
        };
        assert!(scan(&root, &[]).is_empty());
        let png = png();
        install("shanju", MANIFEST, Some(("icon.png", &png)));
        install("broken", "{", None);
        install("settings", MANIFEST, None);
        // 没有 manifest 的目录（例如系统应用的 WebKit 数据）、不合法的目录名都不是应用。
        std::fs::create_dir_all(root.join("browser-data")).unwrap();
        std::fs::create_dir_all(root.join("Upper")).unwrap();
        let first = scan(&root, &["settings".into()]);
        assert_eq!(first.len(), 1);
        let app = &first[0];
        assert_eq!(app.id, "shanju");
        let icon = app.icon.as_ref().unwrap();
        assert_eq!((icon.format, icon.maskable), (IconFormat::Png, true));
        assert_eq!(diff(&[], &first), vec![Change::Added(app.clone())]);
        assert!(diff(&first, &first).is_empty());

        // 重写 manifest（内容不变）不算变化；换了图标算变化。
        let reserved = ["settings".to_string()];
        install("shanju", MANIFEST, None);
        assert!(diff(&first, &scan(&root, &reserved)).is_empty());
        std::fs::write(root.join("shanju/icon.png"), b"not an image").unwrap();
        let second = scan(&root, &reserved);
        assert!(second[0].icon.is_none());
        assert_eq!(
            diff(&first, &second),
            vec![Change::Updated(second[0].clone())]
        );

        // 新增一个、移除一个。
        install(
            "zhi",
            r#"{"name": "纸", "start_url": "https://zhi.example/"}"#,
            None,
        );
        let third = scan(&root, &reserved);
        assert_eq!(
            third.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["shanju", "zhi"]
        );
        uninstall(&root, "shanju").unwrap();
        assert!(!root.join("shanju").exists());
        let fourth = scan(&root, &reserved);
        assert_eq!(
            diff(&third, &fourth),
            vec![Change::Removed("shanju".into())]
        );
        // 卸载不存在的应用不算错误；不合法的 id 拒绝。
        uninstall(&root, "shanju").unwrap();
        assert!(uninstall(&root, "../x").is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn launch_command_is_quoted() {
        let mut app = parse_manifest("127-0-0-1-8765-app", MANIFEST).unwrap();
        app.name = "O'Brien's 笔记".into();
        let cmd = launch_command(&app, "loong-webapp");
        assert_eq!(
            cmd,
            "loong-webapp --id '127-0-0-1-8765-app' --name 'O'\\''Brien'\\''s 笔记' \
             --scope 'http://127.0.0.1:8765/app/' --theme-color '#2e5e74' \
             --background-color '#f3eee2' 'http://127.0.0.1:8765/app/index.html?source=pwa'"
        );
    }
}
