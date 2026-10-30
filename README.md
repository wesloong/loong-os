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

以下画面由 `loong-shell` 在无屏幕模式下渲染（Mesa llvmpipe，渲染路径与真机相同）：

| 主屏 | 打开应用（图标弹出成窗口） | 回主屏手势（窗口跟手缩小） |
|---|---|---|
| ![主屏](docs/images/home.png) | ![打开应用](docs/images/app-opening.png) | ![回主屏手势](docs/images/go-home-gesture.png) |

完整系统在 QEMU 中启动后的画面（UEFI → systemd-boot → UKI → initramfs → 只读 erofs → OpenRC → `loong-shell` 走 DRM/KMS）：

![QEMU 中的 Loong OS](docs/images/qemu-home.png)

## 当前状态：M0 基础设施

| 模块 | 状态 |
|---|---|
| 文档：技术规划、ADR、设计规范 | ✅ 已完成 |
| `tools/dev-chroot.sh`、`tools/build-pkg.sh`：Alpine 3.24 构建 chroot 与 APK 打包 | ✅ 已完成 |
| `crates/loong-motion`：弹簧、惯性衰减、橡皮筋、速度跟踪、吸附/分页、CSS `linear()` 缓动 | ✅ 已完成（含单元测试） |
| `system/packages/wpewebkit`：WPE WebKit 2.54.2 APKBUILD 与 CI 工作流 | ✅ 配方完成，正在 chroot 中编译验证 |
| `crates/loong-ui`：设计令牌、连续圆角、文字（中文）、图标、毛玻璃、壁纸 | ✅ 已完成 |
| `crates/loong-shell`：headless 后端、主屏、打开应用 / 回主屏手势动画、真实 Wayland 客户端（xdg-shell）、真机后端（DRM/KMS + libinput + seatd） | ✅ 已可用（在 QEMU 中验证） |
| QEMU 镜像：`system/mkimage`、`tools/qemu.py`、initramfs、erofs 只读根、A/B 启动与启动计数 | ✅ 可启动到主屏，触摸与回主屏手势可用 |

完整路线图（M0–M7）见[技术规划](docs/技术规划.md#五路线图)。

## 仓库结构

| 路径 | 内容 | 状态 |
|---|---|---|
| `docs/` | 技术规划、架构决策记录（ADR）、设计规范 | 已有 |
| `crates/loong-motion/` | 物理动画引擎（纯数学，无第三方依赖） | 已有 |
| `crates/loong-ui/` | 绘制、设计令牌、连续圆角（超椭圆） | 已有 |
| `crates/loong-shell/` | Wayland 合成器、系统 UI、手势识别 | 已有（M1 进行中） |
| `crates/loong-sysd/` | 系统服务（D-Bus）：背光、电源、Wi‑Fi、OTA | 计划（M4） |
| `webrt/` | C/meson：`webapp/`、`browser/`、`platform/`（`loong` WPEPlatform 后端）、`loongkit/` | 计划（M2） |
| `system/packages/wpewebkit/` | WPE WebKit 的 APKBUILD | 已有 |
| `system/packages/loong-*/` | `loong-shell`、`loong-webrt`、`loong-base`、`loong-initramfs`、`loong-device-*` | 计划 |
| `system/rootfs/` | OpenRC 服务：`loong-session`（系统界面）、`loong-boot-ok`（A/B 启动确认） | 已有 |
| `system/profiles/` | 机型配置与设备描述：`qemu-x86_64` 已有；`xiaomi-nabu`（M3） | 部分已有 |
| `system/initramfs/`、`system/mkimage/` | busybox initramfs；镜像构建：apk → rootfs → erofs → initramfs → UKI → GPT 磁盘 | 已有（UKI）；boot.img（M3） |
| `tools/dev-chroot.sh`、`tools/build-pkg.sh` | 开发 chroot、APK 构建 | 已有 |
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

# 5. 构建 QEMU 系统镜像并启动（在 chroot 内运行 QEMU，TCG 软件模拟，较慢）
sudo tools/dev-chroot.sh run sh -c 'cd /src && cargo build --release -p loong-shell && \
  system/mkimage/mkimage.sh qemu-x86_64 && \
  python3 tools/qemu.py start build/image/qemu-x86_64/loong-qemu-x86_64.img && \
  python3 tools/qemu.py wait "login:" && python3 tools/qemu.py shot out/qemu.png'
#    之后可以：python3 tools/qemu.py tap 235 124；swipe 640 795 640 420；stop

# 其他命令
sudo tools/dev-chroot.sh shell     # 进入 chroot
sudo tools/dev-chroot.sh umount    # 卸载 chroot 的挂载点
```

可以通过环境变量调整 chroot：`CHROOT`（目录）、`ALPINE_BRANCH`（默认 `v3.24`）、`MIRROR`（Alpine 镜像地址）。

## 文档

- [技术规划](docs/技术规划.md)：技术选型、系统架构、多机型与手机形态、路线图、资源目标、风险
- [架构决策记录（ADR）](docs/adr/README.md)：基础发行版、Shell、Web 内核、启动与 OTA
- [设计规范（Loong HIG）](docs/design/设计规范.md)：颜色、字体、圆角、材质、动效、布局、手势、LoongKit 令牌

## 许可证

除特别说明外，本项目采用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可，使用者可任选其一。第三方组件（例如 WPE WebKit：LGPL-2.0-or-later AND BSD-2-Clause）沿用各自的许可证。
