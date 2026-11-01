# 系统应用的 JS bridge（`window.loong`）

“设置”等系统应用也是 Web 应用，运行在 `loong-webapp` 里。它们通过 `loong-webapp` 提供的**受限 JS bridge** 调用系统服务 [loong-sysd](loong-sysd.md)（D-Bus `org.loong.System1`），并读写系统外观。普通 PWA、浏览器里打开的网页拿不到这个 bridge。

源码：[`webrt/webapp/loong-bridge.c`](../../webrt/webapp/loong-bridge.c)（WebKit 与 D-Bus 的衔接）、[`loong-bridge-core.c`](../../webrt/webapp/loong-bridge-core.c)（白名单、参数校验、JSON，纯函数）、[`loong-bridge.js`](../../webrt/webapp/loong-bridge.js)（注入页面的 JS）、[`loong-appearance.c`](../../webrt/webapp/loong-appearance.c)（系统外观）。第一个使用者是[设置](../../webrt/apps/settings/)。

```
设置页面 JS ──webkit.messageHandlers.loong.postMessage──▶ loong-webapp（UI 进程，C）──GDBus（异步）──▶ loong-sysd（root）
   ▲                                                       │  页面地址复核 + 白名单 + 参数校验              │
   ├──── Promise（{ok, value} / {ok:false, error}）◀────────┘◀──── 返回值 / D-Bus 错误 ───────────────────┘
   └──── 事件（属性变化、网络列表变化、低电量、服务启停、外观变化）◀── 信号 ────────────────────────────────┘
```

## 谁能拿到 bridge

同时满足以下条件才会注入 `window.loong`：

1. **启动参数**：`loong-webapp --bridge=system`。这个参数只写在只读根分区的 `/usr/share/loong/apps.toml`（由 `system/profiles/<机型>.apps.toml` 安装）里，用户装的 PWA 由系统界面按注册表启动，永远不带这个参数。`--bridge` 的值只能是 `system`。
2. **起始页在受信任目录内**：起始 URL 必须是 `file://`，路径经 %-解码、`realpath`（消除 `..`、符号链接）后位于受信任目录 `/usr/share/loong/web/` 之下，且文件存在。受信任目录在只读的 erofs 根分区上。目录在构建时确定（meson 选项 `web_root`，产品构建保持默认）。

不满足时 `loong-webapp` **拒绝启动**（退出码 2，打印原因），而不是悄悄降级成普通应用——配置错误应当立即暴露。

```toml
# system/profiles/qemu-x86_64.apps.toml
[commands]
settings = "loong-webapp --id settings --name 设置 --bridge=system file:///usr/share/loong/web/apps/settings/index.html"
```

## 安全模型

| 层次 | 措施 |
|---|---|
| 页面 | JS 只注入**顶层框架**（`WEBKIT_USER_CONTENT_INJECT_TOP_FRAME`），并带 URL 允许列表 `file:///usr/share/loong/web/*`。启用 bridge 的应用里，**任何框架**（主框架、iframe、重定向）只能停留在受信任目录内，或是 `about:blank` / `about:srcdoc`；其他跳转一律拦截（用户点的 http(s) 链接交给系统浏览器），所以能执行脚本的只有系统自带的页面。 |
| 每条消息 | 处理前再核对一次当前页面地址是否仍在受信任目录内（缓存上一次通过的地址），不在就返回 `NotAllowed`；事件也只投递给受信任的页面。 |
| 白名单 | 只认下表中的接口与方法；不暴露 `Properties.Set`、内省、`net.connman.iwd.Agent`、loong-sysd 以后新增的接口（例如 OTA）以及总线上的任何其他服务。**不做通用的 D-Bus 透传。** |
| 参数 | 按方法签名逐个校验：`d` 必须是有限的 number（NaN、Infinity、带 `valueOf` 的对象都拒绝），`b` 必须是 boolean，`s` 必须是 string（UTF-8、不含 NUL、≤ 256 字节）；个数必须一致。 |
| 进程 | 只有 UI 进程（可信的 C 代码）连接系统总线。网页内容运行在 WebContent 进程里，WPE 包保留了 bubblewrap 沙箱（`BUBBLEWRAP_SANDBOX`），沙箱里没有 `/run/dbus/system_bus_socket`。 |
| 系统服务 | loong-sysd 仍按 uid 复核写操作（`access.rs`：只允许 root 与 `loong`），总线策略文件 `org.loong.System1.conf` 同样只对 `loong` 放行写方法——bridge 不是唯一的一道关。 |
| 页面代码 | Wi‑Fi 名称（SSID）由附近的任何人决定，设置页面一律用 `textContent` 显示，从不拼接 HTML。 |

已知的局限：uid 只能区分“会话用户”和“其他用户”。所有 Web 应用都以 `loong`（uid 1000）运行，一个被攻破的 UI 进程可以直接调用 loong-sysd 的写方法；bridge 防的是**网页内容**，不是本地进程。以后如需更细的权限，可以让系统应用使用单独的 uid/组，或由 loong-sysd 核对调用进程（见 [loong-sysd.md](loong-sysd.md#设置应用如何调用)）。

## JS API

`window.loong` 是冻结的、不可改写的对象（`version: 1`）。所有方法返回 Promise。

### `loong.system`：loong-sysd

`loong.system.backlight` / `power` / `wifi` 各对应 loong-sysd 的一个接口，都是 `EventTarget`，有以下通用成员：

| 成员 | 说明 |
|---|---|
| `getAll()` | 读取全部属性。属性名从 D-Bus 的 PascalCase 转成 camelCase：`TimeToEmpty` → `timeToEmpty`，`MaxRaw` → `maxRaw`，`AcOnline` → `acOnline`，`Ssid` → `ssid` |
| `props` | 最近一次读到的属性（同步；还没读过时为 `null`） |
| `watch(callback, onError?)` | 先读一次全部属性并调用 `callback(props, null)`，之后每次变化调用 `callback(props, changedKeys)`。第一次读取失败（例如 loong-sysd 没有运行）时调用 `onError(error)`；服务启动后会自动补发。返回取消订阅的函数 |
| `'change'` 事件 | `detail` 是变化了的属性（来自 `PropertiesChanged`，一次刷新中的多项变化合并在一个事件里） |

各接口的方法（属性的含义、单位见 [loong-sysd.md](loong-sysd.md)）：

| 调用 | D-Bus | 说明 |
|---|---|---|
| `backlight.setBrightness(value, { animate = true })` | `Backlight.SetBrightness(d, b)` | 0–1；`animate` 时 loong-sysd 用弹簧平滑过渡，拖动滑块时可以连续调用 |
| `power.refresh()` | `Power.Refresh()` | 立即重新读取电池 |
| `wifi.getNetworks()` | `Wifi.GetNetworks()` | `[{ ssid, strength, signalDbm, security, known, connected }]`，按信号从强到弱 |
| `wifi.setEnabled(enabled)` | `Wifi.SetEnabled(b)` | |
| `wifi.scan()` | `Wifi.Scan()` | 立即返回；结束后触发 `'networkschanged'` |
| `wifi.connect(ssid, passphrase = '')` | `Wifi.Connect(s, s)` | 连上或失败后才返回（超时 60 秒）；开放网络与已保存的网络传空字符串 |
| `wifi.disconnect()` | `Wifi.Disconnect()` | |
| `wifi.forget(ssid)` | `Wifi.Forget(s)` | |
| `<接口>.getAll()` | `Properties.GetAll` | 只读 |

其他事件：

| 事件 | 来源 | `detail` |
|---|---|---|
| `power` 的 `'lowbattery'` | `Power.LowBattery(s, d)` | `{ level: 'low' \| 'critical', percentage }` |
| `wifi` 的 `'networkschanged'` | `Wifi.NetworksChanged()` | `{}`，重新调用 `getNetworks()` 即可 |
| `loong.system` 的 `'servicechange'` | `org.loong.System1` 的所有者变化 | `{ running }`；重新启动后，各接口自动重新读取属性并通知 `watch` |

```js
const { backlight, power, wifi } = loong.system;
backlight.watch((p) => (slider.value = p.brightness));
await backlight.setBrightness(0.6);
power.watch((p, changed) => render(p), (e) => showError(e));
wifi.addEventListener('networkschanged', async () => render(await wifi.getNetworks()));
try {
  await wifi.connect('茶寮-5G', password);
} catch (e) {
  if (e.name === 'org.loong.System1.Error.Failed') askAgain(); // 多半是密码错误
}
```

### `loong.appearance`：系统外观

| 成员 | 说明 |
|---|---|
| `get()` | `'light'`（宣纸）或 `'dark'`（夜墨） |
| `set(theme)` | 写入系统外观文件（见下文）；本进程立即生效，其他应用与系统界面通过监视文件跟着切换 |
| `theme` | 最近一次的值 |
| `'change'` 事件 | `detail.theme`；无论是谁改的（本应用、别的进程直接改文件）都会触发 |

### `loong.device.info()`：本机信息（只读）

在线程里读取，不经过 D-Bus：

| 字段 | 来源 |
|---|---|
| `os` | `/etc/os-release`：`{ name, prettyName, id, version, versionId, imageVersion, buildId, homeUrl }`（缺的字段不出现） |
| `device` | `/usr/share/loong/device.toml` 原样转成对象：`{ name, size_class, screen: { width, height, diagonal_inch, refresh_hz, scale, … } }`；文件不存在时没有这个字段 |
| `hostname` | 主机名 |
| `kernel` | `uname`：`{ name, release, machine }` |
| `cpu` | `{ count, model?, soc? }`：`/proc/cpuinfo` 的 `model name`/`Hardware`，`/sys/devices/soc0/{family,machine}` |
| `board` | `/proc/device-tree/model`（ARM 设备，例如 `Xiaomi Pad 5`） |
| `memory` | `{ total }`（字节，`/proc/meminfo`） |
| `storage` | `{ data: { total, free, available }, system: { … }, separate }`：`data` 是 `$HOME` 所在的文件系统（设备上是 data 分区），`system` 是 `/`（只读 erofs，`free` 为 0）；`separate` 表示两者是不同的文件系统 |
| `webkit` | WPE WebKit 版本，例如 `2.54.2` |

### 错误

失败的 Promise 以 `loong.LoongError` 拒绝：`name` 是错误名，`message` 是给人看的中文说明（可以直接显示，**判断请用 `name`**）。

| `name` | 含义 |
|---|---|
| `org.loong.System1.Error.*` | loong-sysd 的错误，原样转交（`AccessDenied`、`NotSupported`、`Unavailable`、`InvalidArgs`、`NotFound`、`Busy`、`Failed`），见 [loong-sysd.md](loong-sysd.md#通用约定) |
| `org.freedesktop.DBus.Error.*` | 总线的错误，例如 `ServiceUnknown`（loong-sysd 没有运行）、`AccessDenied`（被策略文件拒绝）、`NoReply`（超时） |
| `org.loong.Bridge.Error.NotAllowed` | 接口或方法不在白名单中，或页面已经离开受信任目录 |
| `org.loong.Bridge.Error.InvalidArgs` | 参数类型、个数不对 |
| `org.loong.Bridge.Error.Unavailable` | 连不上 loong-sysd 所在的总线 |
| `org.loong.Bridge.Error.Failed` | 其他失败（例如写外观文件失败） |

## 系统外观的约定

外观保存在会话用户的配置目录里，**loong-shell 与所有 Web 应用读取同一个文件**：

```toml
# $XDG_CONFIG_HOME/loong/appearance.toml（默认 ~/.config/loong/appearance.toml，设备上是 /home/loong/.config/loong/appearance.toml）
theme = "dark"    # "light"（宣纸）或 "dark"（夜墨）
```

- **默认**：文件不存在、读不了、`theme` 缺失或不是这两个值之一时，按 `"light"` 处理（「墨韵」默认亮色）。读取方只认 `theme` 这一个键，忽略其他内容（以后可能增加键，例如自动切换的时间表）。
- **写入**：只由“设置”（经 bridge）写入，方式是写临时文件再 `rename`（`g_file_set_contents_full`，`CONSISTENT`），读取方不会读到半个文件。文件权限 0644，目录 0700。
- **生效**：
  - 每个 `loong-webapp` 启动时读取，并监视这个文件（inotify，`rename` 也能收到）。变化时更新 WPE 的 `WPE_SETTING_DARK_MODE`，页面的 `prefers-color-scheme` 随之切换——**所有 PWA 都跟随系统外观**，不只是系统应用；同时更新 WebView 的底色（宣纸 `#f3eee2` / 夜墨 `#15140f`），避免加载时闪白。
  - 启用了 bridge 的应用另外收到 `loong.appearance` 的 `'change'` 事件。设置页面切换时在 `<html>` 上设 `data-theme` 并用 View Transitions 交叉晕染。
  - **loong-shell**（`crates/loong-shell/src/appearance.rs`，设备后端）：启动时读取该文件选择 `Theme::Light` / `Theme::Dark`，并用 inotify 监视所在目录（`rename` 替换也能收到），变化时重新生成状态栏、主屏、Dock、键盘、多任务等系统界面的位图。优先级：环境变量 `LOONG_THEME` > 外观文件 > `device.toml` 的 `theme` > 亮色。无屏幕后端（测试脚本）不读这个文件，外观由脚本的 `theme` 决定，保证截图可复现。切换时还没有交叉淡化（直接换下一帧）。

## 底层消息格式

JS 层（`loong-bridge.js`）封装了下面的协议，页面一般不直接使用。

请求（`webkit.messageHandlers.loong.postMessage(请求)` 返回 Promise）：

| 请求 | 说明 |
|---|---|
| `{ op: 'getAll', iface: 'Backlight' \| 'Power' \| 'Wifi' }` | 返回 D-Bus 原名（PascalCase）的属性对象 |
| `{ op: 'call', iface, method, args: [...] }` | 白名单中的方法；有返回值时 `value` 是返回值元组（数组），否则为 `null` |
| `{ op: 'appearance.get' }` / `{ op: 'appearance.set', theme }` | |
| `{ op: 'device.info' }` | |

回复：`{ ok: true, value }` 或 `{ ok: false, error: { name, message } }`。GVariant 转 JSON 的规则：数字 → number（NaN/无穷大 → `null`），字符串、对象路径 → string，`a{sv}` → 对象，其他数组与元组 → 数组，变体 → 内部值。

事件：loong-webapp 调用 `window.loong[Symbol.for('loong.bridge.dispatch')](json)`（JSON 作为参数传入，不拼接进脚本），`json` 形如：

```json
{"iface":"Backlight","signal":"PropertiesChanged","args":{"Brightness":0.9},"invalidated":[]}
{"iface":"Wifi","signal":"NetworksChanged","args":{}}
{"iface":"Power","signal":"LowBattery","args":{"level":"low","percentage":14.0}}
{"iface":"System","signal":"ServiceChanged","args":{"running":true}}
{"iface":"Appearance","signal":"Changed","args":{"theme":"dark"}}
```

## 开发与测试

- **演示数据**：页面里没有 `window.loong`（普通浏览器）或地址带 `?mock` 时，设置改用 [`mock.js`](../../webrt/apps/settings/mock.js)，接口形状与真 bridge 相同。场景：`?mock=1`（充电、已连 Wi‑Fi）、`battery`、`low`、`wifi-off`、`desktop`（没有电池、背光、网卡）。演示网络的密码是 `loong1234`。截图：`node tools/web-shot.mjs --only settings`。
- **开发环境变量**（只影响开发，不扩大权限）：`LOONG_SYSD_BUS=session|system|<地址>`（与 loong-sysd 相同），`LOONG_OS_RELEASE`、`LOONG_DEVICE_FILE`（`device.info()` 读的文件），`LOONG_WEBAPP_CONSOLE=1`（把页面的 `console` 输出写到标准输出）。
- **受信任目录指向源码树**：`meson setup build/webrt-dev webrt -Dweb_root=$SRC/webrt`。
- **单元测试**：`meson test -C build/webrt`（白名单、参数校验、JSON、URL 校验——`..`、`%2e%2e`、符号链接、同前缀的兄弟目录——以及 os-release/TOML 解析）。
- **端到端**：[`webrt/tests/settings-headless.sh`](../../webrt/tests/settings-headless.sh) 在开发 chroot 里搭一个假 sysfs（背光 + 电池）、私有总线上的 loong-sysd、无屏幕的 loong-shell，用 WPE WebKit 打开设置，拖动亮度滑块、从外部改亮度、切换外观、翻看电池与关于本机并截图，同时记录假 sysfs 中 `brightness` 文件的变化。加 `FAKE_IWD=1` 时再起一个假 iwd（[`webrt/tests/fake-iwd.c`](../../webrt/tests/fake-iwd.c)，模拟 loong-sysd 读取的 iwd 对象树与 Agent 回调），走一遍加入已保存的网络、网络详情、忽略网络（确认弹窗）、弹出密码 sheet。
  - chroot 的根不是挂载点，bubblewrap 起不来，脚本因此关掉了 WebKit 的沙箱（只在开发 chroot 中）；同时不设 `DBUS_SESSION_BUS_ADDRESS`，与设备上的会话一致。
  - 无屏幕脚本只有点按与滑动，没有键盘事件，所以在 WebKit 里没法真正输入密码；“输入密码 → Agent 交给 iwd”的流程由 loong-sysd 的集成测试（`crates/loong-sysd/tests/dbus.rs`）与演示数据覆盖。

## 尚未实现

- 页面被冻结（后台）时暂停投递事件，恢复时补发一次最新状态。
- 电量历史（设置的电池页只在演示数据中画 24 小时曲线；需要 loong-sysd 记录）。
- 屏幕键盘尚未接入 WebKit（text-input-v3），真机上输入 Wi‑Fi 密码要等 loong-ime 接入。
- 隐藏网络、802.1X 交互式认证（loong-sysd 本身也不支持）。
