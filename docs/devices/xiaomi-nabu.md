# 小米平板 5（nabu）适配说明

- **状态**：`android-abl` 启动后端已实现，`xiaomi-nabu` 镜像可以完整构建，并通过了离线自检（见“已验证的内容”）。**还没有在真机上启动过**——本沙箱没有这台设备，以下刷机流程尚未实测，首次上机请按“风险”一节做好准备。
- **相关**：[ADR 0004 启动与 OTA](../adr/0004-启动与-ota-ab.md)、[ADR 0001 基础发行版](../adr/0001-基础发行版-alpine.md)、机型配置 `system/profiles/xiaomi-nabu.conf`

## 硬件与软件来源

| 项 | 内容 |
|---|---|
| SoC | 骁龙 860（SM8150-AC），Adreno 640（Mesa freedreno） |
| 屏幕 | 11 英寸 2560×1600，120Hz（面板原生方向为竖屏 1600×2560） |
| 存储 | UFS 3.1，逻辑扇区 4096 字节 |
| 内核 | `linux-postmarketos-qcom-sm8150`（6.17，pmOS 维护的 sm8150-mainline 分支）。UFS、device-mapper、ext4、squashfs 都是内建的；**没有 erofs 和 dm-verity**（见下文） |
| 固件 | `firmware-xiaomi-nabu`（依赖 `linux-firmware-qcom`/`-ath10k`/`-qca`） |
| 软件源 | Alpine 3.24 + postmarketOS（已更名 Nura）v26.06：`https://mirror.nura.eco/postmarketos/v26.06`，以 `@pmos` 标签添加，只有显式写成 `<包名>@pmos` 的包才从这里装 |
| 仓库公钥 | `system/keys/build.postmarketos.org.rsa.pub`，取自 pmaports `main/postmarketos-keys`（v26.06 分支），sha512 与其 APKBUILD 一致；APKINDEX 签名用它验证通过 |

我们**不安装** `device-xiaomi-nabu` 和 `postmarketos-base`。`xiaomi-nabu.conf` 中的 `DEVICE_PKGS` 相当于 `loong-device-xiaomi-nabu` 包的依赖，只有：

| 包 | 用途 |
|---|---|
| `linux-postmarketos-qcom-sm8150@pmos`、`firmware-xiaomi-nabu@pmos` | 内核与固件 |
| `qbootctl` | A/B slot 查询、切换、标记启动成功（由 `loong-boot-ok` 调用） |
| `tqftpserv`、`pd-mapper@pmos`、`rmtfs`（及各自的 `-openrc`） | 远程处理器需要的 QRTR 服务：ADSP（音频）、CDSP、modem（Wi‑Fi 芯片 WCN3990 由 modem 子系统承载） |
| `alsa-ucm-conf` + `system/devices/xiaomi-nabu/rootfs/usr/share/alsa/ucm2/` | 音频路由（nabu 的 UCM 文件来自 pmaports，作者 map220v，MIT） |
| `alsa-utils` + 机型目录里的 `loong-audio` 服务 | 开机/声卡出现时用 `alsaucm` 启用 UCM 的 `HiFi` 用例与 `Speaker`；`aplay`、`amixer`、`speaker-test` 供上机调试，见下文“音频” |
| `mesa-loong`（`MESA_FLAVOR=loong`，见下文“构建 mesa-loong”） | 自编的 Mesa：freedreno GPU 驱动（Adreno 640）+ EGL/GLES，不依赖 LLVM |

## 启动链与分区布局

```
ABL（高通 Android 引导程序）
 └─ boot_a / boot_b：Android boot.img（header v0）
      kernel  = Image.gz + sm8150-xiaomi-nabu.dtb（附加在末尾）
      ramdisk = Loong initramfs（busybox + dmsetup.static + loong-dynpart）
      cmdline = console=ttyMSM0,115200 console=tty0 quiet loglevel=3 psi=1 loong.slot=a|b
       └─ initramfs：
            1. 在 GPT 中按分区名找到 super
            2. loong-dynpart 解析 super 中的 LP 元数据，用 dm-linear 映射出 /dev/mapper/system_a、system_b
            3. 只读挂载 /dev/mapper/system_<slot>（squashfs，见“根文件系统”）
            4. 挂载 userdata（ext4）为 /data，绑定 /var、/home、/root
            5. switch_root → OpenRC → loong-shell
```

| 分区（GPT 名） | 大小 | 内容 |
|---|---|---|
| `boot_a` / `boot_b` | 128 MiB | `boot_a.img` / `boot_b.img`，只有 cmdline 中的 `loong.slot` 不同 |
| `dtbo_a` / `dtbo_b` | 32 MiB | **擦除**。Android 的 dtbo 叠加到主线 dtb 上会导致无法启动（与 pmOS 相同） |
| `super` | 9126805504 字节 | `super.img`：LP 元数据（2 个元数据槽）+ 逻辑分区 `system_a`、`system_b`（组 `loong`），两个 slot 初始内容相同 |
| `userdata` | 剩余空间（100 GB 以上） | `userdata.img`：512 MiB 的 ext4（sparse），首次启动由 `loong-growfs` 在线扩展到整个分区 |
| `vbmeta_a` / `vbmeta_b` | — | 默认不动；仅在 ABL 因 AVB 拒绝启动时刷入 `vbmeta-disabled.img` |

boot.img 参数来自 pmaports 的 `deviceinfo`：pagesize 4096，base 0x0，kernel 0x8000，ramdisk 0x01000000，second 0xf00000，tags 0x100，未指定 header_version（即 v0）。`super` 大小来自 nabu 的 Android 设备树（`BOARD_SUPER_PARTITION_SIZE := 9126805504`），`flash.sh` 刷机前会用 `fastboot getvar partition-size:super` 再核对一次。

### 根文件系统：暂用 squashfs

pmOS 的 sm8150 内核配置（pmaports `config-postmarketos-qcom-sm8150.aarch64`）里 `CONFIG_EROFS_FS` 和 `CONFIG_DM_VERITY` 都没有开启，只内建了 squashfs（支持 lz4/xz，不支持 zstd）。因此 `xiaomi-nabu.conf` 设置了 `ROOT_FS=squashfs`：根文件系统用 `mksquashfs -comp lz4 -Xhc` 生成，同样只读、压缩，A/B 流程不变。mkimage 现在会检查 `INITRAMFS_MODULES` 里的每一项在内核中确实存在（内建或模块），缺了就报错，这个问题就是这样发现的。

要回到 ADR 0004 的 erofs + dm-verity，需要一个开启 `CONFIG_EROFS_FS=y`（M5 还要 `CONFIG_DM_VERITY=y`）的内核：向 pmaports 提交配置修改，或者基于它的 APKBUILD 打包我们自己的 `linux-loong-sm8150`。届时把 `ROOT_FS` 改回 `erofs` 即可。

压缩先保持 lz4hc：这个内核的 squashfs 解压器是单实例（`CONFIG_SQUASHFS_DECOMP_SINGLE=y`），xz 虽然能把根再缩小约 30%，但多核不能并行解压，冷启动 WebKit 会明显变慢。各种参数的大小见 [体积预算](../体积预算.md#压缩参数)。

### 内核模块与固件

还没上真机，`xiaomi-nabu.conf` 只排除肯定用不上的：`KERNEL_MODULES` 去掉别家 SoC 的 GPU/显示/视频编解码、Freescale 音频、Intel/Marvell/RSI 网卡和红外遥控键位表（828 个里去掉 156 个，约 4 MB）；`FIRMWARE` 去掉 `linux-firmware-qcom` 里其他芯片的子目录、ath11k，以及 WCN3990 以外的 ath10k 固件（237 MB → 41 MB）。nabu 自己的固件（ADSP/CDSP/modem、GPU zap、venus、WLAN）都在 `qcom/sm8150/xiaomi/nabu/`，Adreno 640 还要 `qcom/` 顶层的 `a630_sqe.fw` 与 `a640_gmu.bin`，蓝牙在 `qca/`，功放在 `cirrus/`，触摸在 `novatek/`，都保留。

上机后用 `lsmod` 和内核日志里的固件请求（`dmesg | grep -i firmware`）把两者改成白名单。

### 为什么根放在 super

为什么根放在 `super` 而不是像 pmOS 那样整盘写进 `userdata`：`super` 是现成的、与用户数据分开的系统区，大小足够放两个 slot（每个 slot 是根文件系统镜像大小的 1.5 倍 + 16 MiB）；`userdata` 只放可写数据，这样 OTA 写入非活动 slot 时不会碰到用户数据，也与 ADR 0004 的布局一致。代价是刷入 `super.img` 后 Android 无法再启动（见“风险”）。

## 构建

在 x86_64 开发机上（需要 root）：

```sh
sudo tools/setup-binfmt.sh                                    # 注册 qemu-aarch64 binfmt（每次开机一次）
sudo tools/dev-chroot.sh run tools/build-aarch64.sh            # 交叉编译 aarch64 的 loong-shell、loong-sysd、loong-update
sudo ARCH=aarch64 JOBS=2 tools/build-pkg.sh mesa-loong         # aarch64 的 mesa-loong（约 1 小时，见下文；只需一次）
sudo ARCH=aarch64 JOBS=2 tools/build-pkg.sh gst-plugins-loong  # 带 WebKit（WITH_WPE=1）时要：音视频插件
sudo ARCH=aarch64 JOBS=2 tools/build-pkg.sh ffmpeg-loong       # 同上：H.264/AAC 软解（经 qemu-user 约 13 分钟）
sudo tools/dev-chroot.sh run system/mkimage/mkimage.sh xiaomi-nabu
# 带 WebKit 与 Web 运行时（设置、浏览器）的完整镜像：先交叉编译 aarch64 的 wpewebkit 与 webrt（见下文）
sudo JOBS=2 tools/cross-build-pkg.sh wpewebkit                 # 只需一次
sudo tools/cross-build-webrt.sh
sudo tools/dev-chroot.sh run env WITH_WPE=1 system/mkimage/mkimage.sh xiaomi-nabu
```

字体、ICU 数据、拼音词库、`gst-plugins-loong` 等本地包也要有 aarch64 版本（`build/packages/packages/aarch64/`，见 README）。git worktree 的 `build/` 是独立的，mkimage 只看本 worktree 的 `build/packages/`：可以把主仓库里已经打好的 aarch64 包复制过来，再重建索引（`apk index --rewrite-arch aarch64 -o APKINDEX.tar.gz *.apk && abuild-sign APKINDEX.tar.gz`，在开发 chroot 里运行）。

### 构建 mesa-loong（aarch64）

nabu 的图形栈是自编的 `mesa-loong`（`system/packages/mesa-loong/`，Mesa 26.1.6）：gallium 驱动只有 freedreno（Adreno 640，kmsro 随之构建）、virgl 和 softpipe，接口只有 EGL + GLES 2/3（Wayland、GBM、surfaceless），没有 LLVM、X11/GLX、Vulkan。它不在 Alpine 仓库里，要先打包进本地仓库 `build/packages/packages/aarch64/`；不想自编时可用 `MESA_FLAVOR=distro system/mkimage/mkimage.sh xiaomi-nabu` 换回 Alpine 的 mesa（多出 LLVM 等约 195 MiB）。

在 x86_64 开发机上，`ARCH=aarch64 tools/build-pkg.sh` 用一个 aarch64 的 Alpine 构建 chroot（`build/chroot-aarch64/`，第一次运行时自动创建：Alpine 基础系统 + `alpine-sdk`，约 270 MiB，之后复用）经 qemu-user（binfmt）本地编译，产物用开发 chroot 的同一把密钥签名，mkimage 直接信任：

```sh
sudo tools/setup-binfmt.sh                                    # 每次开机一次
sudo ARCH=aarch64 JOBS=2 tools/build-pkg.sh mesa-loong         # 中间文件在 build/abuild/mesa-loong-aarch64/
ls build/packages/packages/aarch64/                            # mesa-loong、-egl、-gles、-gbm、-glx-stub、-dev
```

编译器、meson、Python 代码生成都经 qemu-user 模拟运行：在 4 核的开发沙箱里（`-j2`、`nice 19`，与其他构建共用 CPU）完整构建用了 56 分钟（同一台机器上 x86_64 版本约 7 分钟）。实测结果：

- meson 配置：`Gallium drivers: freedreno virgl softpipe`，EGL 平台 `wayland surfaceless drm`，LLVM、Vulkan、GLX 均为关闭；1266 个编译步骤全部通过。
- 安装后约 15.1 MiB（`libgallium-26.1.6.so` 15.7 MB，含 freedreno 的设备表，`FD640` 即 Adreno 640）；依赖只有 libdrm、libwayland-client、zlib、zstd、libstdc++。
- 按 nabu 的软件包集合（Alpine + pmOS 仓库，另加 gst-plugins-base）模拟安装：`mesa-loong` 方案共 485 MiB，Alpine mesa 方案 680 MiB（多出 llvm22-libs、spirv-tools、mesa-gl 等）；`gst-plugins-base` 对 `libGL.so.1` 的依赖由 `mesa-loong-glx-stub` 满足，不会拉回发行版的 mesa。
- 还没有在真机上运行：freedreno 在 Adreno 640 上的渲染、`loong-shell` 的 GBM/KMS 路径以及 WebKit 走 GPU（dmabuf）都待 M3 上机验证。

也可以用 meson 交叉编译（clang `--target=aarch64-alpine-linux-musl` + `apk --root --arch aarch64` 准备的 sysroot，同 `tools/build-aarch64.sh` 的做法），编译器原生运行会快得多（未实测），但还要自己处理 APK 的打包与签名；只在需要频繁修改 Mesa 时才值得。

### 交叉编译 WPE WebKit（aarch64）

WebKit 不能像 mesa-loong 那样在 aarch64 构建 chroot 里经 qemu-user 编译：x86_64 本机编译 4 核要 3–6 小时，模拟运行编译器还要再慢一个数量级（要好几天）。`tools/cross-build-pkg.sh` 在 x86_64 开发 chroot 里交叉编译：clang、lld 原生运行，只是目标是 aarch64，产物和 `tools/build-pkg.sh` 一样进本地仓库、用同一把密钥签名：

```sh
sudo JOBS=2 tools/cross-build-pkg.sh wpewebkit                     # 从头构建：fetch verify unpack prepare build rootpkg index
sudo JOBS=2 tools/cross-build-pkg.sh wpewebkit build rootpkg index  # 中断后继续：不重新解包，ninja 接着编译
sudo tools/cross-build-pkg.sh wpewebkit clean                       # 打包后删除中间文件与 sysroot（APK 保留）
sudo tools/cross-build-webrt.sh test                                # loong-webapp、loong-browser → build/webrt-aarch64/，经 qemu-user 跑单元测试
```

| 部分 | 做法 |
|---|---|
| sysroot（`build/cross/wpewebkit-aarch64/sysroot/`，约 1 GB） | `tools/cross/mksysroot.sh`：`apk --root <sysroot> --arch aarch64 add --scripts=no`，装 musl-dev、gcc（`crt*.o`、libgcc）、libstdc++-dev 和 APKBUILD 的 `makedepends_host`（Alpine aarch64 仓库，签名用 `/usr/share/apk/keys/aarch64/` 校验）。不运行安装脚本，不需要模拟运行任何程序。指向绝对路径的符号链接改成相对的。mesa-dev 用 Alpine 的（与 x86_64 包的构建环境相同；WebKit 只链接 `libgbm.so.1`，EGL/GLES 由 libepoxy 运行时加载，都由 mesa-loong 提供） |
| 编译器与工具（`tools/cross/aarch64-alpine.cmake`） | `clang`/`clang++ --target=aarch64-alpine-linux-musl`、`CMAKE_SYSROOT`、`-fuse-ld=lld`；ar/ranlib/strip/objcopy 用 Alpine 的 `binutils-aarch64`。头文件、库、CMake 包只在 sysroot 里找，程序只在宿主机上找；pkg-config 用 `PKG_CONFIG_LIBDIR` + `PKG_CONFIG_SYSROOT_DIR` 只看 sysroot |
| 构建时运行的程序 | 全部用开发 chroot 里的：Perl、Python、Ruby（JavaScriptCore 的 offlineasm；LLInt 的偏移量与设置是 Ruby 从交叉编译出的 `LLIntOffsetsExtractor`、`LLIntSettingsExtractor` 二进制文件里读出来的，不运行它们）、gperf、unifdef、`glib-compile-resources`、`glib-mkenums`、`wayland-scanner`。WebKit 没有要先编出来再在构建机上运行的工具 |
| abuild | 以 `CHOST=aarch64 CBUILDROOT=<sysroot>` 运行（`-d`：依赖已由上面装好），即 Alpine 引导新架构时用的交叉模式：`CARCH=aarch64`、CFLAGS 加 `--sysroot`、用 `aarch64-alpine-linux-musl-strip` 去符号、按 sysroot 里装的包推算 `so:`/`pc:` 依赖。默认编译参数取自 aarch64 的 abuild（`ABUILD_DEFCONF`：与 x86_64 相同，只是没有 x86 才用的 `-fno-plt`） |
| APKBUILD | `makedepends` 分成 `makedepends_build`（构建机上的工具）与 `makedepends_host`（链接的库），即 Alpine aports 的写法；`CBUILD` ≠ `CHOST` 时补上 CMake 在交叉编译时探测不到的 `BWRAP_EXECUTABLE`、`DBUS_PROXY_EXECUTABLE`（目标系统上的路径）。体积参数和功能开关与 x86_64 的 r1 完全相同 |
| Web 运行时（`tools/cross-build-webrt.sh`） | meson 交叉编译（`tools/cross/aarch64-alpine.meson`），sysroot 另装本地仓库里的 aarch64 `wpewebkit-dev`；mkimage 构建 aarch64 镜像时从 `build/webrt-aarch64/` 取 `loong-webapp` 与 `loong-browser` |

实测（4 核的开发沙箱，`JOBS=2`、`nice 10`，同时有其他代理在编译，负载 6–8）：

- 从解包到打包完 **3 小时 28 分**（解包与 cmake 配置共约 1 分钟，8876 个编译步骤，打包与建索引不到 1 分钟），与本机编译 x86_64 版本（4 核 3–6 小时，见 ADR 0003）是同一量级；sysroot 第一次准备约 25 秒（324 个 aarch64 包，装好后约 1 GB）。`build/cross/wpewebkit-aarch64/` 最后共 5.4 GB（源码与目标文件约 4.3 GB，不开 ccache；sysroot 约 1 GB），打包后用 `clean` 删掉；webrt 的 sysroot 另有约 0.7 GB。
- 配置与 x86_64 的 r1 相同：7 个 `-O2` 目标、全局 `-Os`、`-fno-asynchronous-unwind-tables`、lld `--icf=safe`；aarch64 上默认开启 JIT、DFG、FTL、WebAssembly。编译参数里只少了 x86 才用的 `-fno-plt`。
- 产物（`build/packages/packages/aarch64/`）：

| | aarch64 | x86_64（同为 r1） |
|---|---|---|
| `wpewebkit-2.54.2-r1.apk` | 30.4 MB（30 388 003 字节） | 31.0 MB |
| 安装大小 | 79.8 MiB | 81.2 MiB |
| `libWPEWebKit-2.0.so.1.11.5`（去符号） | 83.6 MB：`.text` 71.5 MB、`.rodata` 7.7 MB、`.data.rel.ro` 3.1 MB，`.eh_frame` 只剩 1 KB | 85.2 MB |
| `wpewebkit-dev` / `-minibrowser` | 108 KB / 11 KB | 108 KB / 10 KB |

- 检查：库与 `WPEWebProcess`、`WPENetworkProcess`、注入包、MiniBrowser 都是 `ELF 64-bit LSB … ARM aarch64`；44 个 `so:` 依赖（即各文件 `NEEDED` 的外部库）与 x86_64 包相同，只少了 x86_64 上的 `libatomic.so.1`，abuild 按 sysroot 推算出的 `so:` 依赖全部能在 Alpine aarch64 仓库里找到——在一个空的 aarch64 根里 `apk add --simulate wpewebkit mesa-loong-egl mesa-loong-gles mesa-loong-gbm mesa-loong-glx-stub` 解析出 105 个包，没有缺的。
- `tools/cross-build-webrt.sh test`：`loong-webapp`、`loong-browser` 交叉编译约 20 秒，`bridge-core` 单元测试经 qemu-user 通过。
- 整机：带 WebKit 的 `qemu-aarch64` 镜像在 QEMU virt（TCG）里打开设置与浏览器，见下文“已验证的内容”；nabu 镜像的体积见[体积预算](../体积预算.md#nabu)。

产物在 `build/image/xiaomi-nabu/`：

| 文件 | 说明 |
|---|---|
| `boot_a.img`、`boot_b.img` | 两个 slot 的 boot.img |
| `super.img` | sparse 格式的 super 分区镜像（`system_a` + `system_b`） |
| `system.img` | 根文件系统镜像本身（squashfs；以后 OTA 写入非活动 slot 用，首次刷机不需要） |
| `userdata.img` | sparse 格式的 ext4 数据分区 |
| `vbmeta-disabled.img` | 关闭 AVB 校验的空 vbmeta（备用） |
| `flash.sh` | 刷机脚本 |

## 刷机

> **警告**：会覆盖 `super`（Android 系统）和 `userdata`（全部用户数据），并擦除 `dtbo`。刷之前请备份；要回到 Android，需要用 MiFlash 刷官方线刷包。

1. 用小米官方解锁工具解锁 bootloader（需要绑定账号并等待），电量充到 50% 以上。
2. 关机状态下同时按住 **音量下 + 电源**，直到出现 FASTBOOT 画面，用 USB 线连接电脑。
3. 在电脑上（装有 `fastboot`）进入产物目录，运行 `sh flash.sh`。它会先核对设备型号（`product: nabu`）和 `super` 分区大小，不符就停止。等价的手动命令：

```sh
fastboot getvar product                    # 应为 nabu
fastboot getvar partition-size:super       # 应为 0x220000000（9126805504）
fastboot erase dtbo_a
fastboot erase dtbo_b
fastboot flash boot_a boot_a.img
fastboot flash boot_b boot_b.img
fastboot flash super super.img
fastboot flash userdata userdata.img
fastboot --set-active=a
fastboot reboot
```

4. 如果 ABL 拒绝启动（提示校验失败、直接回到 fastboot），再刷关闭校验的 vbmeta 后重启：

```sh
fastboot --disable-verity --disable-verification flash vbmeta_a vbmeta-disabled.img
fastboot --disable-verity --disable-verification flash vbmeta_b vbmeta-disabled.img
```

只更新内核/initramfs 时，重刷当前 slot 的 boot 即可（`fastboot flash boot_a boot_a.img`）；只更新系统时，重新生成并刷 `super.img`（会同时覆盖两个 slot，`userdata` 不受影响）。

## A/B 与回滚（qbootctl）

高通 ABL 把每个 slot 的状态存在 `boot_a`/`boot_b` 的 GPT 分区属性里：active（当前启动哪个）、successful（是否确认过）、unbootable 以及重试次数。没有被标记为 successful 的 slot，ABL 每次启动都会减少它的重试次数；次数用完后把它标为 unbootable，改为启动另一个 slot。

- Loong OS 不启用 qbootctl 自带的 OpenRC 服务（它在开机早期就标记成功）。改由 `loong-boot-ok` 在系统界面连续运行 15 秒后执行 `loong-update mark-good`（即 `qbootctl -m`），与 UEFI 后端去掉启动计数的时机相同；系统界面起不来时重启，消耗一次重试次数。
- 常用命令（在设备上以 root 运行）：

```sh
qbootctl            # 查看两个 slot 的状态
qbootctl -c         # 当前从哪个 slot 启动
qbootctl -m         # 把当前 slot 标记为启动成功
qbootctl -s b       # 下次从 slot b 启动（手动回滚/切换）
qbootctl -u a       # 把 slot a 标记为不可启动
```

- 在 fastboot 中也可以切换：`fastboot --set-active=b`。
- OTA（M5，`loong-update apply`，见 [loong-update](../api/loong-update.md)）：先 `qbootctl -u <另一个 slot>` 作废它，把新系统写入 `/dev/mapper/system_<另一个 slot>`、新 boot.img 写入 `boot_<另一个 slot>`，回读校验后 `qbootctl -s <另一个 slot>`，重启生效。新系统如果没能启动到系统界面，ABL 用完重试次数后会自动回到旧 slot。**这条流程只用假设备和模拟的 qbootctl 做过单元测试，尚未在真机上运行**；重试次数的具体值、`qbootctl -s` 对 successful 标志的处理等 ABL 行为需要在真机上确认。更新包：`UPDATE_BUNDLE=1 system/mkimage/mkimage.sh xiaomi-nabu`。

## 音频（扬声器）

声卡由 ADSP 承载：远程处理器启动、`pd-mapper`/`tqftpserv`/`rmtfs` 经 QRTR 把音频服务注册好之后，ASoC 声卡才出现（驱动名 `sm8150`，长名 “Xiaomi Pad 5”，开机后若干秒）。放音通路是 PCM 设备 0（MultiMedia1）→ `QUAT_TDM_RX_0` → 四个 CS35L41 功放（TL/TR/BL/BR）；麦克风是 WCD9341 经 SLIMbus 到 PCM 设备 1（MultiMedia2）。这些混音器开关默认都是关的，要由 UCM 的 `HiFi` 用例打开（`ucm2/Xiaomi/nabu/HiFi.conf`：接上 MultiMedia1、设功放音量、接上麦克风）——没有这一步，`alsasink` 能打开设备、播放也不报错，但扬声器没有声音。

镜像里的做法（不上 PipeWire，机型目录 `system/devices/xiaomi-nabu/rootfs/` 里四个文件，加 `DEVICE_PKGS` 里的 `alsa-utils`）：

| 文件 | 作用 |
|---|---|
| `/usr/libexec/loong-audio-setup` | 对每块声卡：UCM 里有 `HiFi` 用例就运行 `alsaucm -c hw:<卡号> set _verb HiFi set _enadev Speaker`，其余声卡（USB 声卡等）跳过；结果写进内核日志（`dmesg \| grep loong-audio`，镜像里没有 syslog）。用例设置写进内核的混音器控件，`alsaucm` 退出后仍然有效 |
| `/usr/lib/udev/rules.d/90-loong-audio.rules` | 声卡的控制设备 `controlC<N>` 出现时调用上面的脚本。声卡出现得晚、ADSP 崩溃重启（SSR）后声卡会重新注册并回到默认值，都靠这条规则 |
| `/etc/init.d/loong-audio` | OpenRC 服务（`DEVICE_SERVICES` 里的 `loong-audio:default`，在 udev、pd-mapper 之后、loong-session 之前）：开机时对已有的声卡做一次；手动重做用 `rc-service loong-audio restart` |
| `/usr/share/alsa/cards/sm8150.conf` | ALSA 的 `default` 设备：放音经 `dmix`（PCM 0，48 kHz、S16_LE、立体声），录音经 `dsnoop`（PCM 1），`plug` 负责转换。alsa-lib 按声卡驱动名加载这个文件，不依赖卡号与卡 ID |

**多个网页同时出声**：没有 `sm8150.conf` 时 `default` 直接打开硬件设备，同时只能一个程序放音，第二个网页进程的 `alsasink` 打不开设备、`autoaudiosink` 退回 `fakesink`（静音）。dmix 在进程之间用 SysV 共享内存与信号量，各进程以 `O_APPEND` 共用同一个硬件 PCM，不需要服务进程。WPE WebKit 的网页进程沙箱（bubblewrap）只读绑定 `/etc`、`/usr/share`，绑定 `/dev/snd`，不隔离 IPC 命名空间（`--unshare-ipc` 只在 GTK 的 Wayland 分支里加，见 `BubblewrapLauncher.cpp`），所以所有网页进程、所有应用共用同一个 dmix；loong 用户在 `audio` 组（dmix 的 `ipc_gid`）。离线检查：在开发 chroot 里用 alsa-lib 载入 `sm8150.conf` 并展开 `cards.sm8150.pcm.default:CARD=0`，得到 `asym` → `plug` → `dmix:CARD=0,DEV=0,FORMAT=S16_LE,RATE=48000`（录音同理），没有语法错误。

**QEMU 里的替身检查**（一次性的，脚本没有提交）：给 QEMU x86_64 加一块 HDA 声卡（`ich9-intel-hda` + `hda-output`，声音写进宿主机上的 WAV 文件），从测试盘加载 linux-virt 的 `snd-hda-intel`（镜像的模块白名单里没有声卡），把 `sm8150.conf` 只改驱动名前缀（`sm8150.` → `HDA-Intel.`）后绑定挂载到 `/usr/share/alsa/cards/HDA-Intel.conf`，再开两个 `loong-webapp`（两个沙箱里的网页进程）：

- 第一个网页放 M4A（AAC，770 Hz）：录下的 WAV 里 122 秒都是 770 Hz、幅度与编码时的音量（0.2）相符，除了循环回到开头的那一秒没有断续——AAC 经 `avdec_aac` → `alsasink` → `default` → dmix → 声卡这一路是通的。硬件参数 `S16_LE`、48000 Hz、2 声道、周期 1024 帧（这块 HDA 允许 16 个周期，缓冲 16384 帧）。
- 第二个网页（MP4）在第一个放音的同时打开了 `default`：两个 `WPEWebProcess` 都打开着 `/dev/snd/pcmC0D0p`，dmix 的共享内存段（`ipcs -m`：key `0x0056a4d5`/`0x0056a4d6`，owner loong，权限 660）`nattch` 为 2，第二个进程没有 “Could not open audio device”。没有 dmix 时第二次打开硬件设备会失败（`EBUSY`），`autoaudiosink` 退回 `fakesink`。这证实沙箱里的网页进程能共用一个 dmix。
- 没测到的：两路声音真正叠在一起的录音——loong-shell 只把第一个从命令行启动的应用窗口放到前台，第二个窗口在后台，点按落不到它上面，它的 `play()` 一直等不到用户手势。混音本身是 alsa-lib 的 dmix 做的，上机时按下面第 4 步用两个 `speaker-test` 听。

**代价与以后**：`alsa-utils` 连同依赖（`fftw-single-libs`、`dialog`、ncurses）原大小 4.7 MiB，只读根 +2.3 MiB（见[体积预算](../体积预算.md#nabu)），其中真正用到的只有几十 KB 的 `alsaucm`。适配阶段要用它的 `aplay`、`amixer`、`speaker-test` 上机检查，所以先装着；之后由 loong-sysd 经 alsa-lib 的 `snd_use_case_set()` 设用例（几十行代码），去掉 `alsa-utils`。再往后上 PipeWire + WirePlumber（按 UCM 自动切换扬声器、耳机、蓝牙，音量与路由策略由会话管理器负责）：需要 GStreamer 的 `pipewiresink`（或 PulseAudio 兼容层加 `pulsesink`），并让网页进程的沙箱连到它的 socket（WebKit 目前只绑定 PulseAudio 的 socket），体积另算；那时删掉 `sm8150.conf` 与 `loong-audio`。耳机孔、蓝牙不在这次的范围内。

### 上机检查步骤（尚未在真机上做过）

以 root 在串口或 USB 键盘上运行（`DEV_IMAGE=1` 的镜像 root 无密码）：

```sh
# 1. 声卡出现了吗（要等 ADSP 启动，开机后若干秒）
cat /proc/asound/cards                     # 应有 “sm8150 - Xiaomi Pad 5”，记下卡号 N
dmesg | grep loong-audio                   # 应有 “声卡 N（…）：已启用 HiFi / Speaker”
rc-service loong-audio status
# 没有声卡时：ADSP 与 QRTR 服务
cat /sys/class/remoteproc/*/name /sys/class/remoteproc/*/state
rc-service pd-mapper status; dmesg | grep -iE "q6|apr|gpr|remoteproc|adsp"

# 2. UCM 用例与混音器
alsaucm -c hw:N list _verbs                # HiFi
amixer -c N cget name='QUAT_TDM_RX_0 Audio Mixer MultiMedia1'   # values=on
amixer -c N cget name='TL Analog PCM Volume'                     # 8（TR、BL、BR 同样）
dmesg | grep -i cs35l41                    # 四个功放都探测到、固件加载成功（cirrus/ 下的文件在 FIRMWARE 白名单里）

# 3. 绕过 dmix 直接放音（确认硬件通路；会独占设备，先关掉正在出声的应用）
speaker-test -D hw:N,0 -c 2 -r 48000 -F S16_LE -t sine -f 440 -l 1

# 4. 经默认设备（dmix）两路同时放音：两个音都要听得到，两个命令都不报 “Device or resource busy”
speaker-test -D default -c 2 -t sine -f 440 -l 3 & speaker-test -D default -c 2 -t sine -f 660 -l 3; wait
cat /proc/asound/cardN/pcm0p/sub0/hw_params    # 放音时：format S16_LE，rate 48000，channels 2
ipcs                                            # 放音时有 dmix 的共享内存段与信号量

# 5. ADSP 重启后自动恢复（可选）：让 ADSP 重启，声卡重新出现后 dmesg 里应再有一条 loong-audio 的记录
echo stop > /sys/class/remoteproc/remoteprocX/state; echo start > /sys/class/remoteproc/remoteprocX/state   # X 为 adsp
```

6. 网页：用 `webrt/tests/media/` 的检查页（`mkclips.sh` 生成，经 HTTP 打开，见[媒体格式](../媒体格式.md#qemu-验证)），点按后 WebM、MP4、M4A 应同时出声；再打开浏览器播放另一个网页的视频，两边都有声音。`GST_DEBUG=alsa*:4` 可以看 `alsasink` 打开的是哪个设备。

可能的问题：`speaker-test -D default` 报参数错误时，先试 `plughw:N,0` 确认硬件能放，再把 `sm8150.conf` 里的格式改成 `S24_LE`；默认设备落到了别的声卡（开机时插着 USB 声卡，它成了 0 号）时，用 `ALSA_CARD=<ID>` 或在 `/etc/asound.conf` 里设 `defaults.pcm.card`。

## 已验证的内容（离线，在 x86_64 沙箱中）

- 镜像构建：`mkimage.sh xiaomi-nabu` 完整跑通（rootfs 经 qemu-user 在 aarch64 chroot 中安装，Alpine 与 pmOS 软件源的签名均校验通过）。
- 带 WebKit 的完整镜像：`WITH_WPE=1 mkimage.sh xiaomi-nabu` 跑通（交叉编译的 wpewebkit、loong-webapp、loong-browser，音视频插件与 MP4 软解），只读根 141.4 MiB（squashfs lz4hc），每个 slot 228 MiB，见[体积预算](../体积预算.md#nabu)。
- boot.img：`mkbootimg` 生成后用 `unpack_bootimg` 解包，逐字节核对内核（含附加的 dtb）、initramfs 与 cmdline（构建时自动执行）；header v0，pagesize 与各偏移和 deviceinfo 一致。
- super.img：`simg2img` 还原后用 `lpdump` 检查元数据（`system_a`、`system_b` 各一个 linear extent），并核对 extent 中的数据就是 erofs 根文件系统。
- `loong-dynpart`：用同一个 super 镜像运行，输出的 dm 表与 `lpdump` 一致。
- initramfs 需要的驱动：对照内核包的 `modules.builtin` 与 pmaports 的内核配置，UFS（`ufs-qcom`、`ufshcd-core`、`phy-qcom-qmp-ufs`）、`dm-mod`、`ext4`、`squashfs` 都是内建的，initramfs 里不需要内核模块。
- 根文件系统镜像：在 x86_64 上用 `unsquashfs` 读回检查。
- 同一套 initramfs 脚本和 OpenRC 配置在 QEMU aarch64（`qemu-aarch64` 机型，UEFI 后端，erofs 根）中能启动到锁屏并上滑解锁。
- 交叉编译的 aarch64 WebKit 在 QEMU aarch64 中运行（`WITH_WPE=1` 的 `qemu-aarch64` 镜像，Cortex-A72 TCG，图形栈为 Alpine mesa 的 llvmpipe）：开机到锁屏 47 秒，点 Dock 打开设置 52 秒（经 JS bridge 显示 loong-sysd 给的机型名与 Wi‑Fi 状态），打开浏览器 14 秒（起始页）；`WPEWebProcess` 都在 bubblewrap 沙箱里。截图见 README。WebKit 在 Adreno 640（freedreno）上的 GPU 渲染要等上真机。

## 尚未验证（需要真机）

- ABL 能否引导我们的 v0 boot.img（官方系统用 header v3 + vendor_boot；pmOS 用 v0 能启动，所以预期可以）。
- initramfs 在真机上能否找到 `super`/`userdata`、dmsetup 能否在没有 udev 的环境下建好 `/dev/mapper/system_<slot>`。
- 显示、触控、背光、Wi‑Fi、音频、电池、休眠，以及 120Hz 和屏幕方向。
- GPU：`mesa-loong` 的 freedreno 在 Adreno 640 上的渲染（`loong-shell` 的自定义着色器、WebKit 的 dmabuf 路径）。
- qbootctl 的标记与 ABL 的自动回滚。

## 风险与注意事项

1. **Android 会被覆盖**：刷入 `super.img` 后 Android 的 system/vendor 等逻辑分区都不存在了，不能双系统。恢复只能用 MiFlash 刷官方线刷包（同时会恢复 dtbo、vbmeta）。
2. **dtbo 必须擦除**，否则 ABL 会把 Android 的 dtbo 叠加到主线 dtb 上，内核很可能起不来。
3. **AVB**：bootloader 解锁后 ABL 通常只提示（orange state）而不阻止启动；如果阻止，按上文刷 `vbmeta-disabled.img`。
4. **没有方便的串口**：`console=tty0` 放在最后，initramfs 的报错和救援 shell 会显示在屏幕上，可以接 USB-C OTG 键盘操作。适配阶段的镜像（`DEV_IMAGE=1`）root 没有密码，**不能用于正式发布**。
5. **UFS 稳定性**：pmOS 报告部分三星 UFS 会无故崩溃（`cat /sys/class/block/*/device/model` 可查型号）。
6. **屏幕方向**：面板原生是竖屏 1600×2560，`xiaomi-nabu.device.toml` 按横屏 2560×1600 描述；loong-shell 需要在 M3 中根据 device.toml 加上输出旋转和触控坐标变换。
7. **内核没有 erofs/dm-verity**：见上文“根文件系统”，M5 的完整性校验需要换内核。
8. **镜像体积**：固件依赖拉进了整个 `linux-firmware-qcom`/`ath11k`（`/lib/firmware` 约 230 MB，根文件系统压缩后约 400 MB，QEMU 机型约 200 MB），远超体积目标。需要按 nabu 实际用到的固件裁剪（GPU 只需 `a630_sqe.fw`、`a640_gmu.bin`、`a640_zap.mbn`）。图形栈已换成 `mesa-loong`（安装后约 15 MiB，Alpine 的 mesa 连同 LLVM 等约 195 MiB，见上文“构建 mesa-loong”）。
9. **内核跟随 pmOS**：`linux-postmarketos-qcom-sm8150` 是厂商分支，不是 LTS，每次 pmOS 大版本升级都要重新验证。
10. **userdata 被重新格式化为 ext4**：Android 的 userdata 是加密的 f2fs，刷入后原有数据全部丢失。

## 备选方案：不使用 super（不推荐）

如果将来需要保留 Android（例如双系统），可以像 pmOS 那样把 Loong OS 放进 `userdata`：在 `userdata` 上建一个小 GPT，包含 `root_a`、`root_b`、`data` 三个分区，initramfs 改为在 `userdata` 内部查找分区（需要 `kpartx` 式的映射，同样可以用 dmsetup 实现）。这样 Android 的 `super` 保持不变，但系统与用户数据共用一个物理分区，OTA 时风险更高，暂不采用。
