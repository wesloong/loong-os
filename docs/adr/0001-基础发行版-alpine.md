# ADR 0001：基础发行版选用 Alpine Linux，设备层复用 postmarketOS 内核与固件

- **状态**：已接受
- **日期**：2026-10
- **相关**：[技术规划 · 技术选型](../技术规划.md#一技术选型)、[ADR 0004](0004-启动与-ota-ab.md)

## 背景

Loong OS 的首要目标是“尽可能小、资源占用低”，而且不基于 Android。v1 运行在 ARM64 平板上（参考设备是小米平板 5 nabu），以后还要支持更多机型和手机形态。基础发行版需要满足：

1. **基础系统小**，常驻进程少，没有不必要的守护进程。
2. **预编译包齐全**。v1 需要的组件包括 mesa、libinput、seatd、iwd、pipewire、gstreamer、librime、erofs-utils、systemd-boot/efistub、ukify、Inter 字体，开发还需要 qemu 和 ovmf。不能每一样都从源码构建，因为光 WebKit 一项在 4 核机器上就要编译 3–6 小时。
3. **能直接复用社区的移动设备适配成果**，包括内核、固件和高通相关的用户态小工具。新增机型应该主要是“写设备包 + profile + 实机调试”。
4. **包管理和构建工具简单**，项目自己的包（WPE WebKit、loong-shell、设备包等）能用同一套工具打包。

## 决策

- 采用 **Alpine Linux 3.24**（musl + busybox）作为基础发行版，包格式为 APK，自有包用 `APKBUILD` + abuild 构建（`system/packages/<pkg>/`，通过 `tools/build-pkg.sh` 在 chroot 中构建）。
- 系统组成：**OpenRC** + eudev + **seatd** + D-Bus，不用 systemd/logind；网络使用 **iwd**（内置 DHCP），不用 NetworkManager；音频使用 PipeWire + WirePlumber，媒体使用 GStreamer。
- 设备层复用 **postmarketOS v26.06** 的内核与固件包。v26.06 对应 Alpine 3.24，二进制兼容；镜像已迁至 `mirror.nura.eco/postmarketos/`。
- **不安装** `device-xiaomi-nabu` 和 `postmarketos-base`。自建 `loong-device-<机型>` 包，只依赖内核、固件以及该机型必需的工具（nabu 是 qbootctl、tqftpserv、pd-mapper），并附带 `device.toml` 和音频配置。
- 架构以 aarch64 为主，x86_64 同步构建。开发和 CI 主要用 QEMU x86_64。

## 备选方案及不选原因

| 方案 | 不选原因 |
|---|---|
| **Buildroot** | 镜像可以做得很小，但没有二进制包仓库，所有组件（mesa、gstreamer、pipewire、WebKit 等）都要从源码构建，CI 时间和维护成本都会成倍增加。也不能直接复用 postmarketOS 已经打好的设备内核与固件包 |
| **Yocto / OpenEmbedded** | 灵活且工业界常用，但学习曲线陡，构建时间长、对构建资源要求高，同样要从源码构建全部组件。对小团队来说，维护 layer 的成本超过它带来的可定制性 |
| **直接以 postmarketOS 作为发行版** | 设备支持最好，但 `device-*` 和 `postmarketos-base` 会拖入 pmOS 自己的 initramfs、btrfs/xfs/cryptsetup、sshd、sudo 等，与“尽可能小”和我们的只读 erofs + A/B 启动方案（见 [ADR 0004](0004-启动与-ota-ab.md)）冲突。我们只需要它的内核与固件。pmOS 与 Alpine 二进制兼容，所以“Alpine 做底座 + 选择性复用 pmOS 包”就能拿到它在设备适配上的全部好处 |
| **Debian 系（如 Mobian）** | glibc + systemd 的基础系统更大，常驻服务更多；设备适配也不如 pmOS 覆盖面广 |
| **Android / AOSP，或借助 libhybris 使用 Android 驱动** | 与“不基于 Android”的目标直接冲突。因此只有 Android 厂商内核的机型（多数联发科、麒麟等）不在支持范围内 |

## 影响 / 后果

**正面**

- 基础系统约 8MB，所需组件全部有预编译包，开发 chroot 几分钟就能搭好（`tools/dev-chroot.sh init`）。
- 设备适配可以直接利用 postmarketOS v26.06 的 222 个 aarch64 设备端口；机型准入条件是有主线或接近主线的内核，并有 Mesa 开源 GPU 驱动。
- 只有一种包格式和一套构建工具：自有包和上游包都是 APK，系统镜像由 `apk` 直接安装生成。

**负面与需要自行承担的工作**

- **musl 兼容性**：部分上游软件在 musl 下需要额外处理。Alpine 不提供 WPE WebKit，需要我们自行打包（参考 Chimera Linux 的 musl 配置，并且和 Alpine 的 webkit2gtk 一样关闭 `_FORTIFY_SOURCE`），见 [ADR 0003](0003-web-内核-wpe-webkit.md)。
- **没有 systemd**：用 seatd 代替 logind 的会话/设备访问。Alpine 的 systemd-boot 只提供 `.efi`，没有 `bootctl` 和 `systemd-bless-boot`，所以启动成功标记服务 `loong-boot-ok` 要自己写。OpenRC 服务脚本也由我们维护（`system/rootfs/`）。
- **initramfs 需要自写**：Alpine 的 `mkinitfs` 不支持 erofs/dm-verity，所以要自写 busybox initramfs，见 [ADR 0004](0004-启动与-ota-ab.md)。
- **PSI 默认关闭**：Alpine 内核默认关闭 PSI，cmdline 需要统一加 `psi=1`。
- **mesa 体积**：Alpine 的 mesa 依赖 LLVM，体积大，计划在 M6 自编去掉 LLVM 的 mesa。
- **内核版本跟随 pmOS**：nabu 使用的 `linux-postmarketos-qcom-sm8150`（6.17）是高通厂商 fork，不是 LTS。内核升级节奏取决于 pmOS 的发布，需要在每次 pmOS 大版本升级时重新验证设备。
