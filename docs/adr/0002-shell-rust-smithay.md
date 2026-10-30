# ADR 0002：Shell 采用 Rust + Smithay 自研 Wayland 合成器

- **状态**：已接受
- **日期**：2026-10
- **相关**：[技术规划 · 系统架构](../技术规划.md#二系统架构)、[设计规范 · 动效](../design/设计规范.md#7-动效)

## 背景

Loong OS 的系统 UI（主屏、Dock、状态栏、多任务、控制中心、通知、锁屏、启动闪屏、屏幕键盘、权限弹窗）和窗口合成必须满足：

1. **物理动画是一等公民**：所有动画都由弹簧/惯性/橡皮筋驱动，可被手势随时打断并继承速度，在 nabu 上稳定 120fps。回主屏这类手势要直接驱动**其他进程窗口**的变换，所以动画必须在合成器内部完成。
2. **资源占用低**：shell RSS ≤ 40MB，开机后空闲内存 ≤ 200MB。
3. **许可证兼容**：项目采用 MIT OR Apache-2.0，不能直接使用 GPL 代码。
4. **输入法集成**：合成器内置屏幕键盘 + librime 拼音，通过 `input-method-v2` ↔ `text-input-v3` 与 WebKit 对接。
5. **可测试**：沙箱和 CI 里没有 GPU、没有 KVM，需要能离屏渲染并截图做断言。

## 决策

- 用 **Rust + Smithay** 自研 Wayland 合成器 **`loong-shell`**，在**同一进程**内完成合成、系统 UI、手势识别和应用管理（启动、cgroup freezer 冻结、PSI 内存压力回收）。
- 以 Smithay 自带的 MIT 许可参考合成器 **anvil** 为起点。niri（GPL-3.0）只参考思路，不复制代码。
- **固定 Smithay git 版本**：0.7.0 已发布 16 个月，主干仍很活跃，必要的新功能只在主干上。`default-features = false`，只启用 `backend_egl, backend_drm, backend_gbm, backend_gbm_has_fd_for_plane, backend_gbm_has_create_with_modifiers2, backend_libinput, backend_udev, backend_session_libseat, renderer_gl, renderer_pixman, wayland_frontend, desktop`。
- **绘制模型借鉴 Core Animation**：图层内容用 tiny-skia（矢量）、cosmic-text（文字排版）、resvg（SVG 图标）**只栅格化一次**到纹理，变换、透明度、模糊等动画都由 GLES2 合成完成，动画期间不需要重新栅格化。
- **动画引擎独立成 crate**：`loong-motion` 是纯数学库，没有第三方依赖，由帧时间戳驱动。合成器和 Web 端共用它：Web 端通过 CSS `linear()` 缓动，或用 JS 移植同一套公式。
- **后端分阶段交付**：headless（`EGLSurfacelessDisplay` + `GlesRenderer` 离屏，配合虚拟时钟和脚本化触控）→ udev/DRM/KMS + libinput → winit（在开发机上嵌套运行）。
- **窗口模型**：单任务全屏（类似 iPad），v1 不做自由窗口和分屏。

## 备选方案及不选原因

| 方案 | 不选原因 |
|---|---|
| **Qt Quick + Qt Wayland Compositor（QML）** | Qt 运行时（QtQuick、QML 引擎、QtWayland）体积和内存占用大，与“shell RSS ≤ 40MB”的目标冲突。Qt Wayland Compositor 模块以 GPLv3/商业许可提供，与 MIT/Apache 授权目标冲突。Qt 的动画系统以时长 + 缓动曲线为主，弹簧、打断后继承速度、手势投影仍要自己写。还会引入 C++/QML 双语言栈 |
| **Web 技术实现 shell**（系统 UI 也跑在 WebKit 里） | 每个 WebKit 实例都有可观的内存与启动开销，系统 UI 会和应用争抢资源。手势要经过 JS 主线程，应用卡顿时系统 UI 也可能失去响应。另外，合成和窗口变换动画无论如何都需要一个原生合成器，Web shell 只会多出一层。注意，**系统应用**（设置等）仍然是 Web 应用，那里的性能要求和 shell 不同 |
| **wlroots（C）+ 自写 UI** | wlroots 成熟且是 MIT 许可，但它是 C 语言，内存安全和开发效率不如 Rust。我们的动画引擎和 UI 用 Rust 编写，选 wlroots 会多出一层 FFI 边界 |
| **现成移动 shell（Phosh/phoc、Plasma Mobile/KWin）** | 依赖 GNOME/GTK 或 KDE/Qt 全家桶，体积大；交互与设计语言既定，难以做到全系统物理动画；主要组件为 GPL 许可 |
| **niri 等现成 Rust 合成器二次开发** | niri 是 GPL-3.0，与项目许可冲突；而且它定位是桌面平铺窗口管理，不是触控平板 |
| **使用 Smithay 0.7.0 发布版** | 发布时间太早，主干上已有我们需要的改进。固定 git 版本可以兼顾新功能和可复现性 |

## 影响 / 后果

**正面**

- 动画、手势、合成在同一进程内，能做到“手指 → 窗口变换”的最短路径。打断动画并继承速度是统一的机制（`retarget` / `jump_to`），不需要跨进程协调。
- Rust 带来内存安全，单一语言栈；`loong-motion` 等纯逻辑 crate 可以在宿主机上直接 `cargo test`。
- headless 后端 + 虚拟时钟 + 脚本化触控可以在 CI 中逐帧截图，做像素断言（例如 `--script go-home.json --screenshot-at 0,150,400`）。
- GLES2 是所有目标 GPU 的公约数（freedreno、panfrost、v3d、Intel/AMD，以及 QEMU 下的 llvmpipe/virtio-gpu）。

**负面与风险**

- **工作量大**：系统 UI 组件、屏幕键盘（librime）、`org.freedesktop.Notifications` 服务、loong 私有协议、多任务快照都要自己实现。应对办法是采用单任务全屏模型并分阶段交付（见路线图 M1/M4）。
- **Smithay API 变化频繁**：固定 git 版本后需要有计划地升级，每次升级都要跑 headless 截图回归。
- **没有现成的 UI 控件库**：需要在 `loong-ui` 里实现设计令牌、连续圆角（超椭圆）、文字排版和材质（模糊）。
- 开发机上需要 winit 后端或 QEMU 才能交互调试；CI 中只能使用 headless 后端。
