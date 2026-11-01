# Loong OS

Loong OS 是一款基于 Linux 的平板操作系统。它的目标是尽可能小、尽可能省资源，**不基于 Android**。系统界面是全新设计的，所有系统动画都基于物理模型。应用只有 PWA 一种形态，浏览器使用 WebKit 开源内核。

第一版（v1）面向 ARM64 平板，参考设备是**小米平板 5（nabu）**。手机形态计划在 v2 推出。

## 设计目标

- **小**：底层是 Alpine Linux 3.24（musl + busybox + OpenRC）。根文件系统采用只读 erofs，系统镜像（压缩后）目标 ≤ 250MB，瘦身后 ≤ 150MB。
- **省资源**：开机后空闲内存 ≤ 200MB，shell 常驻内存（RSS）≤ 40MB。后台应用用 cgroup freezer 冻结，内存紧张时根据 PSI 回收。
- **物理动画**：自研 Rust/Smithay 合成器 `loong-shell`，所有动画由弹簧、惯性和橡皮筋模型驱动，并且都能被手势打断、继承手指速度。设计上参考 Apple HIG 的原则，但不使用 Apple 的任何素材。
- **PWA**：应用就是 PWA。每个 PWA 运行在独立的 `loong-webapp` 进程中，数据目录也相互独立。设置等系统应用同样是 Web 应用。
- **WebKit 浏览器**：使用 WPE WebKit 2.54（WPEPlatform API），由项目自行打包。

## 截图

界面采用「墨韵」水墨设计语言（见 [设计规范](docs/design/设计规范.md)、[ADR 0005](docs/adr/0005-设计语言-墨韵.md)）：壁纸、图标、印章全部由程序绘制，动效是“墨在纸上动起来”。以下画面由 `loong-shell` 在无屏幕模式下渲染（Mesa llvmpipe，渲染路径与真机相同）：

| 主屏（山水长卷 + 宣纸小笺图标） | 打开应用（内容像墨在湿宣纸上晕开） | 回主屏手势（收墨，指后留下笔迹） |
|---|---|---|
| ![主屏](docs/images/home.png) | ![打开应用](docs/images/app-opening.png) | ![回主屏手势](docs/images/go-home-gesture.png) |

| 锁屏（文楷时间、农历日期、朱砂印） | 上滑解锁（墨散：锁屏像墨入清水般散开） | 主屏翻页（展卷：山水分层视差，慢滑留下湿笔迹） |
|---|---|---|
| ![锁屏](docs/images/lock.png) | ![上滑解锁](docs/images/unlocking.png) | ![主屏翻页](docs/images/home-paging.png) |

| 多任务（册页卡片 + 墨影） | 暗色「夜墨」主屏 | 暗色锁屏 |
|---|---|---|
| ![多任务](docs/images/switcher.png) | ![暗色主屏](docs/images/home-dark.png) | ![暗色锁屏](docs/images/lock-dark.png) |

“设置”是 Web 应用：WPE WebKit 在 `loong-shell` 里渲染，经 [JS bridge](docs/api/loong-js-bridge.md) 读写 loong-sysd（下图的背光、电池来自假 sysfs，Wi‑Fi 来自假 iwd，见 `webrt/tests/settings-headless.sh`）：

| 显示与亮度（滑块直接调节背光） | 夜墨 · 电池 | 无线局域网 |
|---|---|---|
| ![设置：显示与亮度](docs/images/web/settings-wpe-display.jpg) | ![设置：电池（夜墨）](docs/images/web/settings-wpe-battery-dark.jpg) | ![设置：无线局域网](docs/images/web/settings-wpe-wifi.jpg) |

屏幕键盘与拼音输入（M4）：输入法运行在 loong-shell 进程内（librime），应用经 `zwp_text_input_v3` 收到预编辑与上屏文字，键盘升起后窗口让出高度。下图是 WPE WebKit 页面里打出“你好，世界。”（见 [loong-ime](docs/api/loong-ime.md#8-loong-shell-中的实现)）：

| 组字中（拼音在输入框里，候选栏为文楷） | 夜墨 · 上屏后 | 浏览器地址栏（网址用途，英文） |
|---|---|---|
| ![屏幕键盘 · 组字中](docs/images/keyboard-light.png) | ![屏幕键盘 · 夜墨](docs/images/keyboard-committed-dark.png) | ![浏览器地址栏](docs/images/keyboard-browser-url.png) |

完整系统在 QEMU 中启动后的画面（UEFI → systemd-boot → UKI → initramfs → 只读 erofs → OpenRC → `loong-shell` 走 DRM/KMS；镜像带 WPE WebKit、霞鹜文楷与拼音词库）：

| 主屏 | 设置（经 JS bridge 读 loong-sysd） | 在设置里切到「夜墨」，系统界面随之切换 |
|---|---|---|
| ![QEMU 中的 Loong OS](docs/images/qemu-home.png) | ![QEMU 中的设置](docs/images/qemu-settings.png) | ![QEMU 中的暗色主屏](docs/images/qemu-home-dark.png) |

| 浏览器（跟随夜墨） | 在地址栏用屏幕键盘打拼音 | 选「你好」上屏 |
|---|---|---|
| ![QEMU 中的浏览器](docs/images/qemu-browser-dark.png) | ![QEMU 中的候选栏](docs/images/qemu-keyboard-candidates.png) | ![QEMU 中上屏](docs/images/qemu-keyboard-committed.png) |

ARM64（aarch64）镜像在 QEMU virt 虚拟机中启动后的画面（loong-shell 为交叉编译）：

![QEMU aarch64 中的 Loong OS](docs/images/qemu-aarch64-home.png)

## 当前状态：M0–M5 主体完成，等待上真机

M0（基础设施）、M1（系统界面与手势）、M2（Web 运行时与浏览器）、M4（键盘、设置）、M5（OTA）的主体都已完成，并在 QEMU 或无屏幕模式下验证；M3（小米平板 5 适配）的镜像可以构建，还没有上真机。下一步主要是真机验证（触摸、GPU、Wi‑Fi、电池、120Hz、休眠）和瘦身（M6）。

| 模块 | 状态 |
|---|---|
| 文档：技术规划、ADR、设计规范 | ✅ 已完成 |
| `tools/dev-chroot.sh`、`tools/build-pkg.sh`：Alpine 3.24 构建 chroot 与 APK 打包 | ✅ 已完成 |
| `crates/loong-motion`：弹簧、惯性衰减、橡皮筋、速度跟踪、吸附/分页、CSS `linear()` 缓动 | ✅ 已完成（含单元测试） |
| `system/packages/wpewebkit`：WPE WebKit 2.54.2 APKBUILD 与 CI 工作流 | ✅ 已在 chroot 中编译出 APK（约 47MB，安装后 119MB） |
| `crates/loong-ui`：「墨韵」设计令牌（宣纸/夜墨）、连续圆角、文字（中文，霞鹜文楷）、水墨绘画工具（宣纸、毛笔笔触、山水长卷、宣纸小笺图标、印章） | ✅ 已完成 |
| `crates/loong-shell`：headless 后端、主屏（分页）、打开应用 / 回主屏手势动画、多任务（上滑停顿进入、横向滚动吸附、点按打开、上滑关闭）、真实 Wayland 客户端（xdg-shell、子表面、linux-dmabuf v4、viewporter、分数缩放）、「墨韵」水墨动效（点墨、晕染、笔迹、展卷、墨散）、跟随系统外观、真机后端（DRM/KMS + libinput + seatd） | ✅ 已可用（在 QEMU 中验证）；WebKit 走 GPU（dmabuf）要在有 GPU 的真机上验证 |
| QEMU 镜像：`system/mkimage`、`tools/qemu.py`、initramfs、erofs 只读根、A/B 启动与启动计数 | ✅ 可启动到主屏，触摸与回主屏手势可用 |
| aarch64：qemu-user binfmt（`tools/setup-binfmt.sh`）、交叉编译（`tools/build-aarch64.sh`）、`qemu-aarch64` 镜像 | ✅ 在 QEMU virt（TCG）中启动到锁屏 |
| 小米平板 5：`android-abl` 启动后端（boot.img、super 动态分区 A/B、userdata、qbootctl 回滚），见[适配说明](docs/devices/xiaomi-nabu.md) | 🚧 镜像可构建并通过离线自检，尚未上真机 |
| `crates/loong-update` + `org.loong.Update1`：签名 OTA 更新包、写入非活动 slot、启动确认与自动回滚，见 [loong-update](docs/api/loong-update.md) | ✅ 在 QEMU 中验证（0.1.0 → 0.1.1 升级；系统界面起不来的更新 3 次后自动回滚；坏签名/坏哈希在写入前被拒绝）；`android-abl` 只有单元测试，未上真机；dm-verity 待做 |
| `crates/loong-ime`：librime 拼音引擎（简体，词库 8MB）、iPad 式屏幕键盘模型（布局、命中测试、触摸状态机） | ✅ 已完成（含真实 librime 测试） |
| 屏幕键盘（M4）：loong-shell 内的输入法、「墨韵」键盘与候选栏、`zwp_text_input_v3`、窗口避让、镜像中的预编译 Rime 数据（`loong-rime-data`） | ✅ 在无屏幕 loong-shell + WPE WebKit（loong-webapp、loong-browser）中端到端验证；还没上真机 |
| `webrt/`：`loong-webapp`（每个应用一个进程、跟随系统外观）、系统应用的受限 JS bridge（`window.loong` → loong-sysd，见[说明](docs/api/loong-js-bridge.md)）、**设置**（无线局域网、显示与亮度、电池、关于本机） | ✅ 在无屏幕 loong-shell + WPE WebKit 中端到端验证（亮度滑块写入背光、电池读自 sysfs）；Wi‑Fi 用假 iwd 验证（还没有在真实网卡上试过） |
| `webrt/platform`、`webrt/browser`：自写 `loong` WPEPlatform 后端（每个 view 一个 `wl_subsurface`）与系统浏览器 `loong-browser`（界面 + 网页同屏、标签页、「墨韵」界面、添加到主屏幕），见[下文](#浏览器) | ✅ 在 sway 与 loong-shell（无屏幕）中验证（M2） |

完整路线图（M0–M7）见[技术规划](docs/技术规划.md#五路线图)。

## 仓库结构

| 路径 | 内容 | 状态 |
|---|---|---|
| `docs/` | 技术规划、架构决策记录（ADR）、设计规范 | 已有 |
| `crates/loong-motion/` | 物理动画引擎（纯数学，无第三方依赖） | 已有 |
| `crates/loong-ui/` | 绘制、设计令牌、连续圆角（超椭圆） | 已有 |
| `crates/loong-shell/` | Wayland 合成器、系统 UI、手势识别 | 已有（M1 进行中） |
| `crates/loong-ime/` | 中文拼音输入法：librime 绑定、屏幕键盘模型 | 已有（M4） |
| `crates/loong-sysd/` | 系统服务（D-Bus `org.loong.System1`）：背光、电源、Wi‑Fi（iwd）；另提供系统更新 `org.loong.Update1`。接口见 [docs/api/loong-sysd.md](docs/api/loong-sysd.md) | 已有（M4 进行中） |
| `crates/loong-update/` | A/B 系统更新：签名更新包（ed25519）、写入非活动 slot、启动确认与回滚（`uefi-sdboot` / `android-abl`）。见 [docs/api/loong-update.md](docs/api/loong-update.md) | 已有（M5） |
| `webrt/` | C/meson：`webapp/`（loong-webapp 与 JS bridge）、`apps/`（系统 Web 应用：设置）、`platform/`（`loong` WPEPlatform 后端）、`browser/`（loong-browser 与测试工具）、`loongkit/`、`demo/` | 已有（M2 进行中） |
| `system/packages/wpewebkit/` | WPE WebKit 的 APKBUILD | 已有 |
| `system/packages/loong-rime-data/` | 拼音输入法的 Rime 数据（构建时预编译词典，约 8 MiB） | 已有 |
| `system/packages/loong-*/` | `loong-shell`、`loong-webrt`、`loong-base`、`loong-initramfs`、`loong-device-*` | 计划 |
| `system/rootfs/` | OpenRC 服务：`loong-session`（系统界面）、`loong-boot-ok`（A/B 启动确认） | 已有 |
| `system/profiles/` | 机型配置与设备描述：`qemu-x86_64`、`qemu-aarch64`、`xiaomi-nabu` | 已有（nabu 未上机） |
| `system/devices/<机型>/`、`system/keys/` | 机型专属文件（如 nabu 的音频 UCM）；第三方软件源公钥（postmarketOS/Nura） | 已有 |
| `system/initramfs/`、`system/mkimage/` | busybox initramfs（含动态分区映射 `loong-dynpart`）；镜像构建：apk → rootfs → erofs → initramfs → UKI + GPT 磁盘，或 boot.img + super.img | 已有 |
| `tools/dev-chroot.sh`、`tools/build-pkg.sh` | 开发 chroot、APK 构建 | 已有 |
| `tools/setup-binfmt.sh`、`tools/build-aarch64.sh` | 宿主机注册 qemu-user binfmt；交叉编译 aarch64 的 Rust 组件 | 已有 |
| `tools/qemu.py` | QEMU 启动、QMP 截图、模拟点按/滑动 | 已有 |
| `.github/workflows/` | `rust.yml`、`wpewebkit.yml` 已有；`image.yml` 计划中 | 部分已有 |

## 开发者快速上手

**环境要求**：一台 Linux 宿主机，有 root 权限（chroot 需要 bind mount），装有 `curl` 和 `tar`。chroot 的架构与宿主机相同（`uname -m`）。如果只在宿主机上跑 `loong-motion` 的测试，有 Rust 1.85 及以上版本即可。

```sh
# 1. 搭建 Alpine 3.24 构建 chroot（默认位置是 build/chroot，仓库挂载在 chroot 内的 /src）
sudo tools/dev-chroot.sh init

# 2. 测试物理动画引擎（宿主机直接运行即可）
cargo test -p loong-motion
#    也可以在 chroot（musl）里运行，与 CI 一致
sudo tools/dev-chroot.sh run sh -c 'cd /src && cargo clippy --workspace --all-targets && cargo test --workspace'

# 3. 打包 WPE WebKit（4 核约需 3–6 小时；JOBS 控制并行数，默认 3）
sudo JOBS=8 tools/build-pkg.sh wpewebkit
#    产物在 build/packages/packages/<arch>/：abuild 用 APKBUILD 上级目录名作仓库名
#    日志与中间文件在 build/abuild/wpewebkit/

# 4. 无屏幕运行系统界面：回放触摸脚本，输出截图和 APNG 动画（GPU 用 Mesa llvmpipe 即可）
sudo tools/dev-chroot.sh run sh -c 'cd /src && cargo build -p loong-shell && \
  build/target-alpine/debug/loong-shell --headless --size 1280x800 \
    --script crates/loong-shell/tests/scripts/open-and-go-home.json --out out/demo'
#    把某个图标绑定到真实 Wayland 程序：--app browser=foot（再加 --realtime）
#    其他脚本：switcher.json（多任务）、home-paging.json（主屏翻页）、unlock.json（锁屏）、
#    keyboard.json（屏幕键盘；用真实 WebKit 客户端打字见 docs/api/loong-ime.md 第 8.1 节）

# 5. 构建 QEMU 系统镜像并启动（在 chroot 内运行 QEMU，TCG 软件模拟，较慢）
sudo tools/dev-chroot.sh run sh -c 'cd /src && cargo build --release -p loong-shell -p loong-sysd -p loong-update && \
  system/mkimage/mkimage.sh qemu-x86_64 && \
  python3 tools/qemu.py start build/image/qemu-x86_64/loong-qemu-x86_64.img && \
  python3 tools/qemu.py wait "login:" && python3 tools/qemu.py shot out/qemu.png'
#    之后可以：python3 tools/qemu.py tap 235 124；swipe 640 795 640 420；stop

# 6. aarch64：先在宿主机注册 qemu-user binfmt（每次开机一次），再交叉编译 loong-shell、构建并启动镜像
#    （aarch64 在 TCG 下比 x86_64 慢得多，启动到锁屏要几分钟）
sudo tools/setup-binfmt.sh
sudo tools/dev-chroot.sh run sh -c 'tools/build-aarch64.sh && system/mkimage/mkimage.sh qemu-aarch64 && \
  python3 tools/qemu.py start build/image/qemu-aarch64/loong-qemu-aarch64.img --arch aarch64'
#    小米平板 5：system/mkimage/mkimage.sh xiaomi-nabu，刷机见 docs/devices/xiaomi-nabu.md

# 7. Web 运行时：构建 loong-webapp、loong-browser 并运行单元测试（需要 chroot 中的 wpewebkit-dev）
sudo tools/dev-chroot.sh run sh -c 'meson setup build/webrt webrt && ninja -C build/webrt -j2 && meson test -C build/webrt'
#    设置的端到端检查：假 sysfs + 私有总线上的 loong-sysd + 无屏幕 loong-shell + WPE WebKit（脚本开头有构建说明）
sudo tools/dev-chroot.sh run webrt/tests/settings-headless.sh out/settings              # 亮度、外观、电池、关于本机
sudo tools/dev-chroot.sh run env FAKE_IWD=1 webrt/tests/settings-headless.sh out/wifi  # Wi‑Fi（假 iwd）
#    在桌面浏览器里改设置的界面（演示数据）：打开 webrt/apps/settings/index.html，或 node tools/web-shot.mjs --only settings
#    浏览器：在无屏幕 sway 里回放场景
sudo tools/dev-chroot.sh run sh webrt/browser/tests/sway-run.sh 场景.txt out/browser http://127.0.0.1:8765/index.html
#    场景文件每行一条命令（click / type / key / wheel / shot …，见脚本开头）；同时提供 webrt/browser/tests/pages/
#    在 loong-shell 里点 Dock 图标启动、回放触摸：webrt/browser/tests/shell-run.sh 脚本.json out/shell [网址]
#    镜像：WITH_WPE=1 system/mkimage/mkimage.sh qemu-x86_64（安装 loong-webapp、loong-browser 与界面）

# 其他命令
sudo tools/dev-chroot.sh shell     # 进入 chroot
sudo tools/dev-chroot.sh umount    # 卸载 chroot 的挂载点
```

可以通过环境变量调整 chroot：`CHROOT`（目录）、`ALPINE_BRANCH`（默认 `v3.24`）、`MIRROR`（Alpine 镜像地址）。

## 浏览器

`loong-browser` 是系统浏览器：界面（工具栏、标签栏、菜单与对话框）和网页内容是两类 WebView，在自写的 `loong` WPEPlatform 后端上各占一个子表面，同屏显示。原理、验证与限制见 [ADR 0003 · 实现记录](docs/adr/0003-web-内核-wpe-webkit.md#实现记录m2loong-后端与浏览器)。

| 宣纸（亮色） | 夜墨（暗色） |
|---|---|
| ![浏览器 · 两个标签页](docs/images/web/browser-tabs-light.png) | ![浏览器 · 暗色](docs/images/web/browser-tabs-dark.png) |

```sh
loong-browser [--theme light|dark] [--search-url 'https://www.bing.com/search?q=%s'] [--windowed 1280x800] [网址…]
```

- 主屏的“浏览器”图标启动它（`system/profiles/qemu-x86_64.apps.toml`）；再次启动时网址交给已运行的浏览器，开新标签页。
- 数据目录 `~/.local/share/loong/browser/`，缓存 `~/.cache/loong/browser/`；搜索网址也可写在 `~/.config/loong/browser.ini` 的 `[browser] search-url=`。
- “添加到主屏幕”把网页的 manifest 规范化后写入 `~/.local/share/loong/webapps/<id>/manifest.json`。
- 调试：`LOONG_BROWSER_DEBUG=1` 打印网页控制台消息；`G_MESSAGES_DEBUG=loong-webrt` 打印后端的输入与能力日志。

**loong-shell 为浏览器提供的能力**：

1. **整棵表面树一起缩放**：多任务卡片、开 / 关应用动画中，根表面与各子表面按输出缩放生成、再以窗口原点整体缩放（`RescaleRenderElement`）；圆角、裁剪与晕染遮罩按整个窗口计算，贴边的子表面带窗口圆角，内部子表面不受影响。
2. **`zwp_linux_dmabuf_v1` v4**：默认 feedback 的 main_device 是合成器渲染所用 GPU 的 render node，WebKit 据此在 GPU 上分配缓冲；合成器本身软件渲染（无屏幕 llvmpipe、QEMU 无 3D）时只公布 v3 格式表，客户端走共享内存。
3. **`wp_viewporter` + `wp_single_pixel_buffer_manager_v1` + `wp_fractional_scale_manager_v1`**：根表面的纯色底不占内存，同样带窗口圆角。
4. **子表面命中测试**：触摸发给手指下的子表面（按窗口此刻的缩放换算坐标），按下的表面收到这根手指之后的移动与抬起（隐式抓取）。
5. **`zwp_text_input_manager_v3`** + 屏幕键盘（M4）：地址栏与页面输入框都能用屏幕键盘输入（`crates/loong-shell/tests/scripts/keyboard-browser.json`）。
6. app_id `loong.browser`（以及 `loong.webapp.<id>`）在窗口第一次提交时对应到主屏的应用。
7. 还缺：GPU 上的 DMA-BUF 路径要在真机（nabu）上实测（开发环境没有 GPU）。

## 文档

- [技术规划](docs/技术规划.md)：技术选型、系统架构、多机型与手机形态、路线图、资源目标、风险
- [架构决策记录（ADR）](docs/adr/README.md)：基础发行版、Shell、Web 内核、启动与 OTA
- [设计规范（Loong HIG）](docs/design/设计规范.md)：颜色、字体、圆角、材质、动效、布局、手势、LoongKit 令牌
- [loong-ime](docs/api/loong-ime.md)：输入法架构、text-input-v3 对接、词库体积与延迟
- [loong-sysd](docs/api/loong-sysd.md)：系统服务的 D-Bus 接口；[JS bridge](docs/api/loong-js-bridge.md)：系统 Web 应用如何调用它、安全模型、系统外观文件的约定
- [loong-update](docs/api/loong-update.md)：OTA 更新包格式、A/B 写入与回滚、`org.loong.Update1`

## 许可证

除特别说明外，本项目采用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可，使用者可任选其一。第三方组件（例如 WPE WebKit：LGPL-2.0-or-later AND BSD-2-Clause）沿用各自的许可证。
