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
| `mesa-dri-gallium` | freedreno GPU 驱动 |

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

### 为什么根放在 super

为什么根放在 `super` 而不是像 pmOS 那样整盘写进 `userdata`：`super` 是现成的、与用户数据分开的系统区，大小足够放两个 slot（每个 slot 是根文件系统镜像大小的 1.5 倍 + 16 MiB）；`userdata` 只放可写数据，这样 OTA 写入非活动 slot 时不会碰到用户数据，也与 ADR 0004 的布局一致。代价是刷入 `super.img` 后 Android 无法再启动（见“风险”）。

## 构建

在 x86_64 开发机上（需要 root）：

```sh
sudo tools/setup-binfmt.sh                                    # 注册 qemu-aarch64 binfmt（每次开机一次）
sudo tools/dev-chroot.sh run tools/build-aarch64.sh            # 交叉编译 aarch64 的 loong-shell
sudo tools/dev-chroot.sh run system/mkimage/mkimage.sh xiaomi-nabu
```

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

## 已验证的内容（离线，在 x86_64 沙箱中）

- 镜像构建：`mkimage.sh xiaomi-nabu` 完整跑通（rootfs 经 qemu-user 在 aarch64 chroot 中安装，Alpine 与 pmOS 软件源的签名均校验通过）。
- boot.img：`mkbootimg` 生成后用 `unpack_bootimg` 解包，逐字节核对内核（含附加的 dtb）、initramfs 与 cmdline（构建时自动执行）；header v0，pagesize 与各偏移和 deviceinfo 一致。
- super.img：`simg2img` 还原后用 `lpdump` 检查元数据（`system_a`、`system_b` 各一个 linear extent），并核对 extent 中的数据就是 erofs 根文件系统。
- `loong-dynpart`：用同一个 super 镜像运行，输出的 dm 表与 `lpdump` 一致。
- initramfs 需要的驱动：对照内核包的 `modules.builtin` 与 pmaports 的内核配置，UFS（`ufs-qcom`、`ufshcd-core`、`phy-qcom-qmp-ufs`）、`dm-mod`、`ext4`、`squashfs` 都是内建的，initramfs 里不需要内核模块。
- 根文件系统镜像：在 x86_64 上用 `unsquashfs` 读回检查。
- 同一套 initramfs 脚本和 OpenRC 配置在 QEMU aarch64（`qemu-aarch64` 机型，UEFI 后端，erofs 根）中能启动到锁屏并上滑解锁。

## 尚未验证（需要真机）

- ABL 能否引导我们的 v0 boot.img（官方系统用 header v3 + vendor_boot；pmOS 用 v0 能启动，所以预期可以）。
- initramfs 在真机上能否找到 `super`/`userdata`、dmsetup 能否在没有 udev 的环境下建好 `/dev/mapper/system_<slot>`。
- 显示、触控、背光、Wi‑Fi、音频、电池、休眠，以及 120Hz 和屏幕方向。
- qbootctl 的标记与 ABL 的自动回滚。

## 风险与注意事项

1. **Android 会被覆盖**：刷入 `super.img` 后 Android 的 system/vendor 等逻辑分区都不存在了，不能双系统。恢复只能用 MiFlash 刷官方线刷包（同时会恢复 dtbo、vbmeta）。
2. **dtbo 必须擦除**，否则 ABL 会把 Android 的 dtbo 叠加到主线 dtb 上，内核很可能起不来。
3. **AVB**：bootloader 解锁后 ABL 通常只提示（orange state）而不阻止启动；如果阻止，按上文刷 `vbmeta-disabled.img`。
4. **没有方便的串口**：`console=tty0` 放在最后，initramfs 的报错和救援 shell 会显示在屏幕上，可以接 USB-C OTG 键盘操作。适配阶段的镜像（`DEV_IMAGE=1`）root 没有密码，**不能用于正式发布**。
5. **UFS 稳定性**：pmOS 报告部分三星 UFS 会无故崩溃（`cat /sys/class/block/*/device/model` 可查型号）。
6. **屏幕方向**：面板原生是竖屏 1600×2560，`xiaomi-nabu.device.toml` 按横屏 2560×1600 描述；loong-shell 需要在 M3 中根据 device.toml 加上输出旋转和触控坐标变换。
7. **内核没有 erofs/dm-verity**：见上文“根文件系统”，M5 的完整性校验需要换内核。
8. **镜像体积**：固件依赖拉进了整个 `linux-firmware-qcom`/`ath11k`（`/lib/firmware` 约 230 MB，根文件系统压缩后约 400 MB，QEMU 机型约 200 MB），远超体积目标。需要按 nabu 实际用到的固件裁剪（GPU 只需 `a630_sqe.fw`、`a640_gmu.bin`、`a640_zap.mbn`）。
9. **内核跟随 pmOS**：`linux-postmarketos-qcom-sm8150` 是厂商分支，不是 LTS，每次 pmOS 大版本升级都要重新验证。
10. **userdata 被重新格式化为 ext4**：Android 的 userdata 是加密的 f2fs，刷入后原有数据全部丢失。

## 备选方案：不使用 super（不推荐）

如果将来需要保留 Android（例如双系统），可以像 pmOS 那样把 Loong OS 放进 `userdata`：在 `userdata` 上建一个小 GPT，包含 `root_a`、`root_b`、`data` 三个分区，initramfs 改为在 `userdata` 内部查找分区（需要 `kpartx` 式的映射，同样可以用 dmsetup 实现）。这样 Android 的 `super` 保持不变，但系统与用户数据共用一个物理分区，OTA 时风险更高，暂不采用。
