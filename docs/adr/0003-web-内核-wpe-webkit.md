# ADR 0003：Web 内核采用 WPE WebKit 2.54（WPEPlatform API），浏览器使用自写 `loong` WPEPlatform 后端

- **状态**：已接受
- **日期**：2026-10
- **相关**：[技术规划 · 关键设计](../技术规划.md#关键设计)、`system/packages/wpewebkit/APKBUILD`

## 背景

Loong OS 不建设原生应用生态，所有应用都是 PWA，浏览器要基于 WebKit 开源内核。Web 内核需要满足：

1. **面向嵌入式、资源占用低**，不依赖任何桌面 UI 工具包。
2. **原生支持 Wayland**，能作为 loong-shell 的普通 Wayland 客户端运行。
3. **输入法**：支持 `text-input-v3`，能和合成器内置的 Rime 屏幕键盘配合。
4. **媒体**：通过 GStreamer 播放音视频，nabu 上走 V4L2 Venus 硬解。
5. **多进程与沙箱**：每个 PWA 一个进程，数据目录相互独立；Web 内容进程要有沙箱。
6. **浏览器要能组合“界面（chrome）+ 网页内容”**：地址栏和标签栏与网页内容需要同屏显示。

另外，Alpine 不提供 WPE WebKit，需要自行打包。

## 决策

- 采用 **WPE WebKit 2.54.2**，使用从 2.54 起稳定并成为默认的 **WPEPlatform API**。开启 Wayland 与 Headless 平台后端，关闭 DRM 后端和旧版 libwpe API（`ENABLE_WPE_LEGACY_API=OFF`）。
- **自行打包 APK**（`system/packages/wpewebkit/APKBUILD`），musl 配置参考 Chimera Linux：
  - 编译：clang + lld，`-O2`（JSC 性能比体积更重要），`-U_FORTIFY_SOURCE`，不开 LTO，有 ccache 时自动启用。
  - 关闭：QT_API、DOCUMENTATION、INTROSPECTION、JOURNALD_LOG、WEBDRIVER、SPELLCHECK、GAMEPAD、SPEECH_SYNTHESIS、SAMPLING_PROFILER、API/LAYOUT_TESTS；`USE_ATK/FLITE/LIBBACKTRACE/LIBHYPHEN/SYSPROF_CAPTURE=OFF`。
  - 保留：BUBBLEWRAP_SANDBOX（运行时依赖 bubblewrap、xdg-dbus-proxy）、GBM、LIBDRM。
  - MiniBrowser 单独打成 `wpewebkit-minibrowser` 子包，仅用于开发测试。
- **运行时组件**放在 `webrt/`（C，meson），直接基于 WPEPlatform 编写：
  - `loong-webapp`：每个 PWA 一个进程，使用独立的 WebKit 数据目录；超出 manifest scope 的导航交给浏览器。
  - `loong-browser`：系统浏览器，负责“添加到主屏幕”（解析 W3C manifest，写入 `~/.local/share/loong/webapps/<id>/`）。
  - 输入法使用 WPE 已有的 `WPEInputMethodContextWaylandV3`（text-input-v3）。
- **自写 `loong` WPEPlatform 后端**（`webrt/platform/`）。调研发现，**WPE 的 Wayland 后端会把一个 toplevel 下的所有 view 画到同一个 `wl_surface` 上**：没有子表面，max-views 默认为 1，浏览器界面和网页内容无法作为两个 view 同屏显示。因此：
  - **首选方案**：`loong` 后端为**每个 view 创建一个 `wl_subsurface`**，并承载 loong 私有协议，传递 app_id、theme-color、状态栏样式，以及启动闪屏交接（合成器绘制的闪屏 → 首帧内容）。webapp 也使用这个后端，以获得私有协议能力。
  - **备选方案**：由 shell 绘制浏览器工具栏，通过私有协议与浏览器进程通信。
  - 这是 **M2 的第一项技术验证**。

## 备选方案及不选原因

| 方案 | 不选原因 |
|---|---|
| **WebKitGTK** | Alpine 有现成的包，但它绑定 GTK 工具包。我们的 UI 不用 GTK，引入 GTK 只会增加体积和内存，而且它面向桌面应用嵌入场景。WPE 是 WebKit 官方面向嵌入式的移植，没有 UI 工具包依赖 |
| **WPE 旧 API：libwpe + WPEBackend-fdo + Cog** | 这是 2.54 之前的方案，多出 libwpe 和后端插件两层间接。2.54 起 WPEPlatform 已稳定并成为默认，自带 Wayland 后端和 text-input-v3 输入法支持。Cog 是通用的单窗口启动器，我们需要自己的运行时（PWA 生命周期、私有协议、数据目录隔离），用不上它 |
| **直接使用 WPE 自带的 Wayland 平台后端** | 一个 toplevel 只有一个 `wl_surface`，无法让界面和内容两个 view 同屏，满足不了浏览器的需求；它也不支持 loong 私有协议（theme-color、状态栏样式、闪屏交接） |
| **Chromium / Blink（CEF、Electron 类方案）** | 不符合“基于 WebKit 开源内核”的产品要求；体积、内存和编译成本也更高 |

## 影响 / 后果

**正面**

- 没有 UI 工具包依赖，WebKit 是 Wayland 客户端，shell 对它和对其他客户端一视同仁。
- 每个 PWA 一个进程、各自独立的数据目录，配合 shell 的 cgroup freezer 冻结和 PSI 回收，资源可控。
- text-input-v3、GStreamer 媒体、bubblewrap 沙箱都是上游现成能力。

**负面与风险**

- **编译成本高**：4 核约需 3–6 小时。GitHub 托管 runner 单个任务最长 6 小时（`wpewebkit.yml` 超时设为 355 分钟）；ARM runner 只对公开仓库免费，私有仓库的 runner 只有 2 核。应对办法：使用 ccache；必要时在开发机或自托管 runner 上构建，再把 APK 发布到 Release。
- **打包与升级由我们维护**：每次升级 WPE 都要重新验证 musl 兼容性和编译选项。
- **自写 WPEPlatform 后端是 M2 的关键风险**：如果首选方案受阻，就退回到由 shell 绘制浏览器工具栏。
- **体积与内存**：WebKit 是系统中最大的组件。M6 计划做 ICU 裁剪（只保留中英文）、设置 WebKit 内存上限、字体子集化。
- **许可证**：WPE WebKit 采用 LGPL-2.0-or-later AND BSD-2-Clause。我们的组件以动态链接方式使用它，保持 MIT/Apache；如果修改 WebKit 本身，需要按 LGPL 提供修改后的源码。
- **架构**：loongarch64 上的 WebKit 没有 JIT，性能需要单独评估。
