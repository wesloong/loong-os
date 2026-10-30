# ADR 0004：只读 erofs + A/B 双分区；自写 initramfs；`uefi-sdboot` 与 `android-abl` 两种启动后端

- **状态**：已接受
- **日期**：2026-10
- **相关**：[技术规划 · 启动与 A/B 的两种后端](../技术规划.md#启动与-ab-的两种后端)、[ADR 0001](0001-基础发行版-alpine.md)

## 背景

v1 包含 OTA A/B 更新。系统要做到：更新可靠（断电或坏包都能自动回滚）、根文件系统小且不可篡改、能在多种机型上启动。具体约束如下：

1. **引导方式因机型而异**：QEMU 和 x86 设备用 UEFI；小米平板 5 用高通 ABL（Android 引导程序），它只认 `boot_a`/`boot_b` 中的 Android `boot.img`，rootfs 位于 `super` 动态分区；以后还会有使用 U-Boot 的 Rockchip、全志、树莓派等机型。
2. **Alpine 的现成工具不够用**：
   - `mkinitfs` 不支持 erofs 和 dm-verity；
   - `linux-virt` 内核中的 erofs、virtio-blk、dm-verity 都是模块，需要由 initramfs 加载；
   - systemd-boot 只提供 `.efi`，没有 `bootctl` 和 `systemd-bless-boot`；系统也不用 systemd（见 [ADR 0001](0001-基础发行版-alpine.md)）。
3. **内存压力回收依赖 PSI**，而 Alpine 内核默认关闭 PSI。
4. **构建环境**：镜像要能在 CI 和沙箱中构建，不应依赖 loop 设备或特权挂载。

## 决策

### 文件系统布局

- 根文件系统使用**只读 erofs（lz4hc 压缩）**，分 A、B 两个 slot；可写数据全部放在 `/data`。
- `/var`、`/etc/machine-id`、`resolv.conf`、`hostname`、`/var/lib/iwd` 等可变内容，由 initramfs 从 `/data` bind 挂载，或用 tmpfs 提供。
- dm-verity 完整性校验在 M5 接入。initramfs 从一开始就按支持 dm-verity 来设计。

### 自写 busybox initramfs（`loong-initramfs`）

- 加载所需模块（erofs、virtio-blk 等），**按分区标签选择 A 或 B 根分区**，从 `/data` 挂载可写目录，然后切换到真正的根文件系统。
- 不使用 Alpine 的 `mkinitfs`，也不使用 postmarketOS 的 initramfs。

### 启动后端（由 profile 中的 `boot_backend` 和 `device.toml` 声明）

| 后端 | 适用 | 启动链 | A/B 切换与回滚 |
|---|---|---|---|
| **`uefi-sdboot`** | QEMU、UEFI 设备 | UEFI → systemd-boot → 每个 slot 一个 **UKI**（`ukify` 生成，内含 cmdline） | systemd-boot 的**文件名启动计数** `+N-M`：剩余次数用完的项被视为坏项，自动改从另一个 slot 启动 |
| **`android-abl`** | 小米平板 5 | ABL → `boot_a`/`boot_b` 中的 Android `boot.img`（`mkbootimg` 生成，附带 dtb）→ `super` 分区内的 `system_a`/`system_b`（由 initramfs 的 `loong-dynpart` 映射，功能同 pmOS 的 `make-dynpart-mappings`）；`/data` 位于 `userdata` | `qbootctl` + ABL 自带的重试计数 |
| `uboot`（后续） | Rockchip、全志、树莓派等 | U-Boot | U-Boot bootcount |

`uefi-sdboot` 的细节：

- 用 mtools 把 systemd-boot 的 `.efi` 和 UKI 拷入 ESP，`loader.conf` 自己写。分区用 sfdisk 生成 GPT：**ESP | root_a | root_b | data**。整个过程不需要 loop 设备。
- 自写 OpenRC 服务 **`loong-boot-ok`**：系统启动完成后，从 efivarfs 读取 `LoaderBootCountPath`，把对应 UKI 文件名中的计数后缀去掉，即“标记本次启动成功”。这需要加载 `efivarfs` 模块。

### 内核参数

- 内核 cmdline 统一加 **`psi=1`**，供 loong-shell 根据内存压力回收后台应用。

### OTA（M5）

- 由 `loong-update` 下载**签名的整镜像更新包**，写入非活动 slot，再通过当前后端的机制切换 slot。新系统如果没能完成启动确认，就由启动计数自动回滚。

## 备选方案及不选原因

| 方案 | 不选原因 |
|---|---|
| **可写根分区 + 原地 `apk upgrade`** | 更新不是原子的，中途断电可能导致系统无法启动；没有可回滚的旧系统；无法做 dm-verity 校验。应用都是 PWA，系统运行时本来也不需要安装软件包 |
| **squashfs 作为只读根** | 能用，但 erofs 是专为只读系统分区设计的（Android 系统分区大量使用），随机读性能和解压开销更好，适合按需加载的 WebKit 等大文件 |
| **使用 Alpine `mkinitfs` 或 postmarketOS initramfs** | `mkinitfs` 不支持 erofs/dm-verity；pmOS initramfs 有自己的分区与挂载逻辑，并且拖入 btrfs/xfs/cryptsetup 等，与我们的 A/B 布局不符 |
| **`bootctl` + `systemd-bless-boot`** | Alpine 不提供（只有 systemd-boot 的 `.efi`），系统也不运行 systemd。我们需要的逻辑只是“读一个 EFI 变量、重命名一个文件”，自写 `loong-boot-ok` 很小 |
| **GRUB** | 实现 A/B 计数要写 grubenv 脚本，体积和复杂度都更高。systemd-boot + UKI 每个 slot 一个文件，自带启动计数，更简单 |
| **所有机型统一一种引导方式**（例如在 nabu 上链式加载 U-Boot/UEFI） | 会增加一层非原生引导，调试和刷机都更复杂。直接用 ABL 原生的 A/B 与重试计数最稳妥 |
| **通用 OTA 框架（RAUC、SWUpdate、OSTree 等）** | 功能完整，但我们的需求只是“整镜像写入另一个 slot + 两种后端切换”。自写 `loong-update` 更小，也便于与 loong-sysd 和签名方案统一。以后如果需要差分更新，再评估是否引入 |

## 影响 / 后果

**正面**

- 根文件系统只读、整镜像更新，系统状态可复现。更新失败时由引导层自动回滚，不依赖系统内的任何服务还能运行。
- 设备差异集中在启动后端和 `device.toml`，shell 与系统服务对所有机型是同一份二进制。
- 镜像构建只需 mtools、sfdisk、erofs-utils、ukify、mkbootimg，可以在 CI 和沙箱中无特权运行。

**负面与需要承担的工作**

- 需要自行维护 initramfs、`loong-boot-ok`、`system/mkimage/` 构建脚本，以及每种后端的 slot 切换逻辑。
- 两个 slot 会让根分区占用的空间翻倍，这也是镜像体积目标（压缩后 ≤ 250MB，瘦身后 ≤ 150MB）重要的原因之一。
- 所有可变状态都必须显式放到 `/data`。新增系统服务时，要检查它的写入路径（加入 bind/tmpfs 列表）。
- 必须验证回滚：在 QEMU 中人为损坏 slot A，确认启动计数会回滚到 slot B；nabu 上的 ABL 回滚在 M3/M5 中验证。
- 在 `android-abl` 后端上，回滚行为取决于 ABL 实现，需要逐个机型确认。
