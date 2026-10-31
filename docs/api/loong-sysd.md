# loong-sysd 接口说明（`org.loong.System1`）

`loong-sysd` 是 Loong OS 的系统服务，以 root 运行，在**系统总线**上占用 `org.loong.System1`，向系统界面（`loong-shell`）和系统 Web 应用（设置等）提供背光、电池/电源、Wi‑Fi 三项能力。它还占用 `org.loong.Update1`，提供 A/B 系统更新（链接 `loong-update` 库），接口见 [loong-update.md](loong-update.md#d-busorgloongupdate1)。

源码：[`crates/loong-sysd`](../../crates/loong-sysd)；总线策略：[`system/rootfs/usr/share/dbus-1/system.d/org.loong.System1.conf`](../../system/rootfs/usr/share/dbus-1/system.d/org.loong.System1.conf)；OpenRC 服务：[`system/rootfs/etc/init.d/loong-sysd`](../../system/rootfs/etc/init.d/loong-sysd)。

| 对象路径 | 接口 | 内容 |
|---|---|---|
| `/org/loong/System1/Backlight` | `org.loong.System1.Backlight` | 屏幕亮度 |
| `/org/loong/System1/Power` | `org.loong.System1.Power` | 电池与电源 |
| `/org/loong/System1/Wifi` | `org.loong.System1.Wifi` | Wi‑Fi（封装 iwd） |
| `/org/loong/System1/Wifi/Agent` | `net.connman.iwd.Agent` | 内部使用：iwd 回调取密码 |

每个对象同时实现标准的 `org.freedesktop.DBus.Properties`、`Introspectable`、`Peer`。

## 通用约定

- **属性全部只读**。修改一律通过方法，这样所有写操作都走同一套权限检查。
- **变化通知**：属性变化时发标准的 `org.freedesktop.DBus.Properties.PropertiesChanged`，一次刷新中的多项变化合并在一个信号里（新值直接带在信号中，不用 invalidated）。客户端应先订阅信号、再读一次全部属性（`GetAll`），之后只靠信号更新。
- **单位**：亮度是 0.0–1.0 的小数；电量是 0.0–100.0；时间是秒（`int64`，0 表示未知）；信号强度是 0–100 的整数。
- **错误**：loong-sysd 自己的错误名为 `org.loong.System1.Error.<名称>`，错误消息是给人看的中文说明（可以直接显示，但不要据此做逻辑判断，判断请用错误名）：

  | 错误名 | 含义 |
  |---|---|
  | `org.loong.System1.Error.AccessDenied` | 调用者无权执行写操作 |
  | `org.loong.System1.Error.NotSupported` | 本机没有这项硬件或不支持该操作（例如没有背光、WEP 网络、802.1X 网络） |
  | `org.loong.System1.Error.Unavailable` | 依赖的系统服务没有运行（iwd） |
  | `org.loong.System1.Error.InvalidArgs` | 参数不合法（例如亮度是 NaN、加密网络没给密码、密码格式不对） |
  | `org.loong.System1.Error.NotFound` | 找不到对象（例如附近没有这个 SSID） |
  | `org.loong.System1.Error.Busy` | 正在执行同类操作 |
  | `org.loong.System1.Error.Failed` | 其他失败，消息中带底层错误（例如 iwd 返回的 `Failed`：通常是密码错误） |

  另外，总线本身可能返回 `org.freedesktop.DBus.Error.AccessDenied`（被策略文件拒绝）、`ServiceUnknown`（loong-sysd 没有运行）等标准错误。

## 权限

| 操作 | 谁可以调用 |
|---|---|
| 读属性（`Get` / `GetAll`）、内省、接收信号 | 所有用户 |
| `Power.Refresh`、`Wifi.GetNetworks` | 所有用户 |
| `Backlight.SetBrightness` | root、`loong`（uid 1000） |
| `Wifi.SetEnabled` / `Scan` / `Connect` / `Disconnect` / `Forget` | root、`loong`（uid 1000） |
| `net.connman.iwd.Agent.*` | 只有当前的 iwd 进程 |

两道检查：

1. **总线策略文件**（`org.loong.System1.conf`）：只有 root 能占用服务名；写方法在 `context="default"` 中被拒绝，再对 `user="loong"` 放行；Agent 接口只对 root 开放。
2. **loong-sysd 内部复核**：每个写方法先用 `org.freedesktop.DBus.GetConnectionUnixUser` 查调用者的 uid，不是 0 或 1000 就返回 `org.loong.System1.Error.AccessDenied`。这样即使策略文件没装好也不会失守。Agent 方法则核对调用者的唯一总线名是否就是 `net.connman.iwd` 当前的所有者，否则返回 `net.connman.iwd.Agent.Error.Canceled`——这一点很重要：Agent 手里有用户刚输入的 Wi‑Fi 密码。

注意：uid 只能区分“系统/会话用户”和“其他用户”。所有 Web 应用都以 `loong` 运行，**区分“设置应用”和“普通 PWA”的责任在 Web 运行时**（见文末）。

## `org.loong.System1.Backlight`

| 属性 | 类型 | 说明 |
|---|---|---|
| `Available` | `b` | 是否找到了背光设备（QEMU、台式机没有） |
| `Brightness` | `d` | 感知亮度 0.0–1.0。过渡动画进行中时是**目标值**，所以滑块不会被拉回去 |
| `MaxRaw` | `u` | 设备的 `max_brightness`（没有设备时为 0） |
| `Device` | `s` | 设备名，例如 `intel_backlight`、`acpi_video0` |

| 方法 | 说明 |
|---|---|
| `SetBrightness(d value, b animate)` | 设置亮度。`value` 超出 0–1 会被截断，NaN/无穷大返回 `InvalidArgs`；没有设备返回 `NotSupported`。`animate=true` 时平滑过渡 |

实现要点：

- **设备选择**：`/sys/class/backlight/*` 中按类型 `firmware` > `platform` > `raw` 选择，同类型按名称排序；没有 `max_brightness` 的条目忽略。可用 `/etc/conf.d/loong-sysd` 里的 `LOONG_BACKLIGHT=<设备名>` 指定。
- **感知映射**：`raw = min + (max − min) × value^2.2`。人眼对亮度的感知近似幂函数，滑块拖到一半时看起来就是“一半亮”。级数很少的设备（`max_brightness` ≤ 32，例如 ACPI 的 0–15）改用线性映射，否则低端几级会挤在一起。
- **保持可见**：`value = 0.0` 对应“最低可见亮度”（raw = max(1, 0.5% × max)），不会把屏幕关黑。设置 `LOONG_BACKLIGHT_ALLOW_OFF=1` 后 0.0 才对应 raw 0。熄屏应由合成器通过 DRM（DPMS）完成，而不是把背光调到 0。
- **平滑过渡**：用 loong-motion 的临界阻尼弹簧（response 0.25 s，不过冲）在感知空间里插值，约 0.35 秒到位，每帧 16 ms 写一次 sysfs（raw 不变则不写）。过渡中再次调用会从当前位置和速度继续——拖动滑块时可以每次 `animate=true` 调用，亮度会顺滑地跟着走。
- **外部修改**：亮度热键等改了 raw 时，内核发 `backlight` uevent，loong-sysd 重新读取并发 `PropertiesChanged`。

## `org.loong.System1.Power`

| 属性 | 类型 | 说明 |
|---|---|---|
| `HasBattery` | `b` | 是否有电池 |
| `Percentage` | `d` | 电量 0.0–100.0（一位小数） |
| `State` | `s` | `charging` / `discharging` / `full` / `not-charging`（接着电源但没充电，例如充电上限保护）/ `unknown` |
| `TimeToEmpty` | `x` | 预计剩余使用时间（秒）。只在 `discharging` 时有值，0 表示未知 |
| `TimeToFull` | `x` | 预计充满时间（秒）。只在 `charging` 时有值，0 表示未知 |
| `AcOnline` | `b` | 是否接着外部电源（充电器/USB/无线充电）。没有电池也没有电源信息时为 true |
| `WarningLevel` | `s` | `none` / `low` / `critical`。只在使用电池时出现 |
| `LowThreshold` | `d` | 低电量阈值，目前固定 15 |
| `CriticalThreshold` | `d` | 极低电量阈值，目前固定 5 |

| 方法 | 说明 |
|---|---|
| `Refresh()` | 立即重新读取（有变化会发 `PropertiesChanged`）。对所有人开放 |

| 信号 | 说明 |
|---|---|
| `LowBattery(s level, d percentage)` | 放电中 `WarningLevel` 升级（`none`→`low`、`low`→`critical` 等）时发一次；接上电源后重新计算。系统界面据此弹出低电量提醒 |

实现要点：

- 数据来自 `/sys/class/power_supply/*`。`type=Battery` 的是电池（`scope=Device` 的触控笔、键盘电池和 `present=0` 的忽略）；`type=Mains/USB/Wireless` 的只看 `online`。
- 电量优先用驱动给的 `capacity`，否则用 `energy_now/energy_full` 或 `charge_now/charge_full` 计算；多块电池按满电容量加权。读不到、格式不对的文件当作缺失，`capacity` 超过 100 会被截断。
- 剩余时间优先用驱动给的 `time_to_empty_now`/`time_to_full_now`；否则用能量 ÷ 功率（µWh ÷ µW，没有 `power_now` 时用 `current_now × voltage_now`）或电荷 ÷ 电流估算。功率做指数平滑（系数 0.3），状态变化时重置；结果按分钟取整，超过 7 天视为未知。
- 驱动报告 `Unknown` 时：接着电源且电量 ≥ 99% 视为 `full`，没接电源视为 `discharging`。
- 每 30 秒轮询一次；内核发出 `power_supply` uevent（插拔充电器等）时 200 ms 内刷新。无法监听 uevent（例如在容器里）时只靠轮询。

## `org.loong.System1.Wifi`

对 [iwd](https://iwd.wiki.kernel.org/)（`net.connman.iwd`）的一层薄封装。有多块网卡时使用对象路径最小、处于客户端（station）模式的那块。

| 属性 | 类型 | 说明 |
|---|---|---|
| `Available` | `b` | iwd 在运行且有 Wi‑Fi 网卡 |
| `Enabled` | `b` | Wi‑Fi 是否开启 |
| `State` | `s` | `unavailable`（iwd 没运行或没有网卡）/ `off` / `disconnected` / `connecting` / `connected` / `disconnecting` / `roaming` |
| `Scanning` | `b` | 是否正在扫描 |
| `Ssid` | `s` | 当前连接的网络名（未连接时为空） |
| `Strength` | `u` | 当前连接的信号强度 0–100（未连接为 0），状态栏图标用 |

| 方法 | 说明 |
|---|---|
| `GetNetworks() → a(suisbb)` | 附近的网络，按信号从强到弱。每项为 `(ssid, strength, signal_dbm, security, known, connected)`：`strength` 0–100（-90 dBm 为 0，-30 dBm 为 100）；`signal_dbm` 为 dBm；`security` 为 `open` / `wep` / `psk` / `8021x`（WPA3-SAE 也是 `psk`）；`known` 表示已保存。同名网络可能出现多次（例如开放和加密各一个）。对所有人开放 |
| `SetEnabled(b enabled)` | 开关 Wi‑Fi（iwd 的 `Device.Powered`；打开时如果网卡 `Adapter.Powered` 是关的也一并打开） |
| `Scan()` | 开始扫描，立即返回；扫描结束后网络列表更新并发 `NetworksChanged`。已经在扫描时不报错 |
| `Connect(s ssid, s passphrase)` | 连接网络，**等到连上或失败才返回**（可能需要几秒）。开放网络和已保存的网络 `passphrase` 传空字符串；给已保存的网络传了新密码时，会先忘掉旧配置再用新密码连接。同名网络优先已保存的，其次信号最强的。错误：附近没有 → `NotFound`；加密网络没给密码 → `InvalidArgs`；WEP、未配置的 802.1X → `NotSupported`；密码错误等 → `Failed` |
| `Disconnect()` | 断开当前连接（不忘记网络） |
| `Forget(s ssid)` | 忘记已保存的网络（删除密码、不再自动连接）。没有保存过 → `NotFound` |

| 信号 | 说明 |
|---|---|
| `NetworksChanged()` | `GetNetworks()` 的结果变了（扫描完成、信号变化、连接状态变化），客户端重新调用 `GetNetworks()` 即可 |

实现要点：

- 后台任务订阅 iwd 发出的所有信号和 `net.connman.iwd` 的 `NameOwnerChanged`，去抖 150 ms 后用 `GetManagedObjects` + `Station.GetOrderedNetworks` 重建快照，比较后发 `PropertiesChanged` / `NetworksChanged`。信号强度没有变化通知，所以另外每 30 秒刷新一次。
- **密码**：`Connect` 把密码按网络对象路径暂存在内存里，然后调用 iwd 的 `Network.Connect`；iwd 回调 Agent 的 `RequestPassphrase` 时交给它；`Connect` 返回后立即丢弃。密码不写日志、不落盘（保存由 iwd 负责，在 `/var/lib/iwd`）。Agent 在 iwd 每次（重新）启动后自动注册。
- **iwd 没有运行时**：`Available=false`、`State=unavailable`，写方法返回 `Unavailable`，`GetNetworks()` 返回空列表；iwd 启动后自动恢复，不需要重启 loong-sysd。
- v1 不支持：隐藏网络（`Station.ConnectHiddenNetwork`）、802.1X 企业网络的交互式认证（已用配置文件预置的可以连接）、WEP（iwd 本身不支持）。

## 调试

镜像中没有 `busctl`，可以用 `dbus-send`：

```sh
# 读全部属性
dbus-send --system --print-reply --dest=org.loong.System1 /org/loong/System1/Power \
  org.freedesktop.DBus.Properties.GetAll string:org.loong.System1.Power
# 设置亮度（root 或 loong）
dbus-send --system --print-reply --dest=org.loong.System1 /org/loong/System1/Backlight \
  org.loong.System1.Backlight.SetBrightness double:0.6 boolean:true
# 附近的 Wi‑Fi
dbus-send --system --print-reply --dest=org.loong.System1 /org/loong/System1/Wifi \
  org.loong.System1.Wifi.GetNetworks
# 看信号
dbus-monitor --system "sender='org.loong.System1'"
```

日志在 `/var/log/loong-sysd.log`（`/etc/conf.d/loong-sysd` 里设 `RUST_LOG=debug` 可以看到更多）。

开发时可以不用 root、不碰真实硬件：`LOONG_SYSFS_ROOT=<目录>` 把 sysfs 指向假目录树，`--bus session` 或 `--bus <地址>` 连到会话总线或私有总线。集成测试就是这样做的：

```sh
tools/dev-chroot.sh run sh -c 'cargo test -p loong-sysd'   # 单元测试 + 私有 dbus-daemon 上的集成测试
```

集成测试（`crates/loong-sysd/tests/dbus.rs`）需要 `dbus-daemon` 与 `dbus-send`（`apk add dbus`，没有时自动跳过）；以 root 运行时还会用 nobody 身份验证权限检查，并加载真正的策略文件，另有一个假 iwd 用来验证 `Connect` → Agent 取密码的完整流程。

## 设置应用如何调用（设计说明，尚未实现）

设置等系统应用是 Web 应用，运行在 `loong-webapp` 进程里（每个应用一个进程，都以 `loong` 运行）。它们通过 `loong-webapp` 提供的**特权 JS bridge** 调用 loong-sysd，普通 PWA 拿不到这个 bridge。

```
设置页面 JS ──postMessage──▶ loong-webapp（UI 进程，C） ──系统总线──▶ loong-sysd（root）
   ▲                              │  白名单 + 参数校验                    │
   └──── Promise / DOM 事件 ◀──────┘◀──── 返回值 / PropertiesChanged ─────┘
```

1. **谁能拿到 bridge**：只有同时满足以下条件的应用才注册 bridge——应用 id 在系统应用列表里（由只读根分区上的 `/usr/share/loong/apps.toml` 标记 `system = true`，用户无法修改），并且起始 URL 位于只读根分区（`file:///usr/share/loong/web/<应用>/`）。第三方 PWA、浏览器里打开的网页一律没有。
2. **JS 侧**：用 WebKit 的带回复脚本消息（`webkit_user_content_manager_register_script_message_handler_with_reply`，在独立的 script world 中），只接受主框架、且来源（origin）在应用 scope 内的消息；iframe 和跳转出去的页面不能调用。LoongKit 在其上封装成 `window.loong.system`：

   ```js
   const { backlight, power, wifi } = window.loong.system;
   await backlight.setBrightness(0.6, { animate: true });
   power.addEventListener("change", (e) => render(e.detail));   // PropertiesChanged
   power.addEventListener("lowbattery", (e) => warn(e.detail.level));
   const nets = await wifi.getNetworks(); // [{ ssid, strength, signalDbm, security, known, connected }]
   try {
     await wifi.connect("Home", password);
   } catch (e) {
     // e.name === "org.loong.System1.Error.Failed" 等
   }
   ```

3. **C 侧（loong-webapp）**：用 GDBus 连接系统总线。**不做通用的 D-Bus 透传**：只有一张固定的白名单（接口、方法、参数签名），逐个校验参数类型和范围后才转发；返回值按 `d/x/u → number`、`b → boolean`、`s → string`、`a(suisbb) → 对象数组` 转换，D-Bus 错误转成 rejected Promise（保留错误名）。bridge 订阅 `PropertiesChanged`、`NetworksChanged`、`LowBattery`，转成页面上的 DOM 事件；页面不可见（被冻结）时不投递，恢复时补发一次最新状态。
4. **为什么不能只靠 loong-sysd 的 uid 检查**：所有 Web 应用都是 uid 1000，loong-sysd 看到的只是“会话用户在调用”。因此：
   - 网页内容运行在 WebContent 进程里（WebKit 自带的 bubblewrap 沙箱，我们的 WPE 包保留了 `BUBBLEWRAP_SANDBOX`），沙箱不能暴露系统总线套接字（`/run/dbus/system_bus_socket`）；只有 UI 进程（可信的 C 代码）连接系统总线。实现 bridge 时需要验证这一点。
   - 以后如需更细的权限（例如只允许设置应用改 Wi‑Fi），可以让 loong-webapp 为系统应用使用单独的 uid/组，或由 loong-sysd 核对调用进程的 `/proc/<pid>/exe` 与应用 id，策略文件和 `access.rs` 相应收紧。
5. **系统界面**：`loong-shell`（Rust）直接用 zbus 连接系统总线，读取状态栏的电量、Wi‑Fi 图标，控制中心的亮度滑块调用 `SetBrightness(value, true)`，收到 `LowBattery` 时弹出提醒。
