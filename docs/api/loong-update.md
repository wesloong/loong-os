# loong-update：A/B 系统更新

`loong-update` 把签名的整镜像更新包写入**非活动 slot**，再让引导程序下次试一次这个 slot。新系统启动后，`loong-boot-ok` 确认系统界面正常运行才“确认”这次启动；没被确认的 slot 在尝试次数用完后由引导程序自动放弃，回到旧系统。设计取舍见 [ADR 0006](../adr/0006-ota-更新包与-loong-update.md)，启动链与 A/B 布局见 [ADR 0004](../adr/0004-启动与-ota-ab.md)。

| 组成 | 位置 |
|---|---|
| 库与命令行 `loong-update` | [`crates/loong-update`](../../crates/loong-update) |
| D-Bus 接口 `org.loong.Update1`（由 loong-sysd 提供，链接同一个库） | [`crates/loong-sysd/src/update.rs`](../../crates/loong-sysd/src/update.rs) |
| 启动确认服务 `loong-boot-ok` | [`system/rootfs/etc/init.d/loong-boot-ok`](../../system/rootfs/etc/init.d/loong-boot-ok) |
| 打包 | `UPDATE_BUNDLE=1 system/mkimage/mkimage.sh <机型>`、[`tools/mkupdate.sh`](../../tools/mkupdate.sh) |
| 机型配置（镜像里） | `/etc/loong/update.conf`（mkimage 生成） |
| 信任的公钥（镜像里） | `/usr/share/loong/update-keys/*.pub` |

## 更新包格式

文件名约定 `update-<版本>-<机型>.loongup`。它就是一个 **ustar 归档**（`tar -tvf` 可以查看），成员顺序固定：

| 成员 | 内容 |
|---|---|
| `manifest.toml` | 清单（≤ 64 KiB） |
| `manifest.sig` | 对 `manifest.toml` 原始字节的 **ed25519 签名**（64 字节） |
| 载荷 | 顺序、文件名、大小与清单中的 `[[payload]]` 一致 |

```toml
format = 1                       # 清单格式；不兼容的修改会增加它，旧程序拒绝新格式
version = "0.1.1"                # 新系统版本（与新系统 /etc/os-release 的 VERSION_ID 一致）
profile = "qemu-x86_64"          # 机型，必须与本机 update.conf 的 PROFILE 相同
arch = "x86_64"
boot_backend = "uefi-sdboot"     # uefi-sdboot | android-abl
min_version = "0.1.0"            # 可选：本机至少要运行这个版本
root_fs = "erofs"                # 可选，说明用
created = "2026-10-31T08:00:00Z"

[[payload]]
kind = "rootfs"                  # 根文件系统镜像，写入目标 slot 的根分区（两个 slot 通用）
file = "rootfs.img"
size = 201326592
sha256 = "…"

[[payload]]
kind = "uki"                     # uefi-sdboot：每个 slot 一个 UKI（cmdline 里写着 loong.slot=<slot>）
slot = "a"
file = "loong-a.efi"
size = 16777216
sha256 = "…"
# … slot b 的 UKI；android-abl 则是 kind = "boot"（boot_a.img / boot_b.img）
```

- **签名覆盖清单，清单覆盖载荷**：清单里有每个载荷的大小和 SHA-256。验签失败时清单根本不会被解析。
- 设备信任 `/usr/share/loong/update-keys/` 下的所有公钥（任意一把验证通过即可），公钥随系统镜像更新，可以借此轮换密钥。
- 密钥是标准 PEM（私钥 PKCS#8、公钥 SubjectPublicKeyInfo），与 OpenSSL 逐字节兼容，所以也可以在离线机器上用 `openssl pkeyutl -sign -rawin -inkey key.pem -in manifest.toml` 签名。

### 生成更新包

```sh
# 开发密钥：第一次运行 mkimage 时自动生成 build/keys/loong-dev.pem（私钥，不进 git）与 .pub（装进镜像）
UPDATE_BUNDLE=1 LOONG_VERSION=0.1.1 tools/dev-chroot.sh run system/mkimage/mkimage.sh qemu-x86_64
#   → build/image/qemu-x86_64/update-0.1.1-qemu-x86_64.loongup
# 同一批载荷重新打包（换密钥、设置最低版本）
LOONG_UPDATE_MIN_VERSION=0.1.0 tools/dev-chroot.sh run tools/mkupdate.sh qemu-x86_64
# 正式发布：镜像只装公钥，私钥不放在构建机上
LOONG_UPDATE_PUBKEY=release.pub tools/dev-chroot.sh run system/mkimage/mkimage.sh xiaomi-nabu
```

`UPDATE_BUNDLE=1` 时 mkimage 把载荷硬链接到 `build/image/<机型>/update/`（uefi-sdboot 另外生成一个带真实版本号的 slot B UKI：磁盘镜像里 slot B 的 `IMAGE_VERSION` 是 0，只作后备），再调用 `tools/mkupdate.sh` 用宿主机的 `loong-update pack` 打包签名，最后完整校验一遍。

## 命令行

```text
loong-update status [--boot-state]       两个 slot 的状态；--boot-state 只输出本次启动 good/trial
loong-update apply [--allow-downgrade] <更新包|URL|->
loong-update verify [--key <公钥>]... <更新包|URL|->   验签 + 校验所有载荷，不写入
loong-update mark-good                    确认本次启动（loong-boot-ok 调用）
loong-update rollback [--force]           下次启动改用另一个 slot
loong-update keygen <私钥.pem> <公钥.pub>   （开发机）
loong-update pack --key … --version … --profile … --arch … --backend … [--min-version …] -o <输出> <种类[@slot]=文件>...
```

更新包来源可以是文件、**块设备**（例如 QEMU 里额外挂的一块盘 `/dev/vdb`）、`-`（标准输入）或 `http(s)://` URL（用 curl 或 busybox wget 下载，边下载边写）。

```text
# loong-update status
机型:       qemu-x86_64（x86_64，uefi-sdboot）
当前:       slot b，Loong OS 0.1.1，本次启动已确认
slot a      0.1.0        已确认  [loong-a.efi]
slot b*     0.1.1        已确认  [loong-b.efi]
下次启动:   slot b
```

退出码：0 成功，1 失败（错误信息最后的括号里是错误种类，见下文），2 用法错误。

## 安装流程

1. 取得锁 `/run/loong/update.lock`（同一时间只有一个更新操作）。**本次启动必须已确认**：试运行中的系统不能覆盖另一个 slot——那是唯一的后备（错误 `Unconfirmed`）。
2. 读清单、验签，检查机型、架构、启动后端、`min_version`，以及版本必须比当前新（`--allow-downgrade` 放宽这一条）。检查目标分区放得下。
3. **来源能读两遍时（文件、块设备）先完整读一遍，校验所有载荷的 SHA-256。签名或哈希不对的包在这一步被拒绝，任何东西都没写。** 来源是下载流或标准输入时跳过这一步（只能读一遍），改为边写边校验。
4. **作废目标 slot**，然后写入。第二遍读的时候哈希再算一次（防止两遍之间文件被换掉）。
5. `fsync`，用 `posix_fadvise(DONTNEED)` 丢掉页缓存，**回读校验**写入的数据。
6. **切换**：让引导程序下次试一次目标 slot。

任何一步失败（包括取消），目标 slot 保持作废状态，当前系统不受影响。

### 两种启动后端

| | uefi-sdboot（QEMU、UEFI 设备） | android-abl（小米平板 5） |
|---|---|---|
| 根文件系统写到 | GPT 分区 `root_<slot>`（按分区名查找） | `super` 里的逻辑分区 `/dev/mapper/system_<slot>`（initramfs 的 `loong-dynpart` 已映射两个 slot） |
| 启动镜像写到 | ESP 上的临时文件 `EFI/Linux/loong-<slot>.efi.new`（不以 `.efi` 结尾，systemd-boot 不会启动它） | `boot_<slot>` 分区 |
| 作废目标 slot | 删除它的 UKI | `qbootctl -u <slot>` |
| 切换 | 临时文件改名为 `loong-<slot>+3.efi`（3 次启动计数）；新 UKI 的 `IMAGE_VERSION` 更高，排在最前 | `qbootctl -s <slot>` |
| 降级（`--allow-downgrade`） | 另外把当前 slot 的 UKI 标成坏的（`+0`），旧版本才会排在前面 | 同上 |
| 确认（`mark-good`） | 读 EFI 变量 `LoaderBootCountPath`，去掉那个 UKI 文件名里的计数 | `qbootctl -m` |
| 自动回滚 | 计数用完（`+0-3`）的 UKI 被 systemd-boot 排到最后 | ABL 重试次数用完后把 slot 标为不可启动，换回另一个 |
| 手动回滚 | 另一个 slot 的 UKI 重新获得 3 次尝试，当前 slot 的标成坏的 | `qbootctl -s <另一个>` |

- ESP 平时不挂载：loong-update 用到时挂在 `/run/loong/esp.<进程号>.<序号>`，用完卸载。
- android-abl 的逻辑分区大小在刷机时已经固定（mkimage 给每个 slot 留了 50% 余量），新镜像放不下时报 `TooLarge`，需要整包刷机；不支持调整逻辑分区大小。
- **android-abl 尚未在真机上验证**，只用假的块设备（临时文件）和模拟 qbootctl 的脚本做了单元测试（`crates/loong-update/src/ops_tests.rs`）。qbootctl 的输出格式按 [linux-msm/qbootctl](https://github.com/linux-msm/qbootctl) 的源码解析。

### 启动确认（`loong-boot-ok`）

OpenRC 服务，在 `loong-session` 之后运行：

1. `loong-update status --boot-state` 不是 `trial`（已确认、或不是 A/B 启动）时什么也不做。
2. 试运行时等 `loong-shell` 进程**连续运行** `LOONG_BOOT_OK_STABLE` 秒（默认 15；进程号变了说明崩溃后被重新拉起，重新计时），然后 `loong-update mark-good`。
3. `LOONG_BOOT_OK_TIMEOUT` 秒（默认 180）内没等到：不确认，并**重启**，让引导程序消耗一次尝试次数（`LOONG_BOOT_OK_REBOOT=0` 时只报错）。

这些变量可以在 `/etc/conf.d/loong-boot-ok` 中修改。为了让坏系统也能自动回滚，initramfs 找不到/挂不上根分区时不再停在救援 shell，而是 10 秒后重启（按回车或内核命令行加 `loong.rescue` 进入救援 shell）；内核命令行带 `panic=10`，内核崩溃也会重启。

## D-Bus：`org.loong.Update1`

由 loong-sysd（root）提供：总线名 `org.loong.Update1`，对象 `/org/loong/Update1`，接口 `org.loong.Update1`。约定与 [loong-sysd](loong-sysd.md#通用约定) 相同：属性只读，变化时发 `PropertiesChanged`；错误名为 `org.loong.Update1.Error.<种类>`，消息是给人看的中文。

| 属性 | 类型 | 说明 |
|---|---|---|
| `Available` | `b` | 本机是否支持 A/B 更新（有 `/etc/loong/update.conf`） |
| `State` | `s` | `idle` / `checking`（验签、完整校验）/ `writing` / `verifying`（回读校验）/ `activating` / `ready`（已写入，重启后生效）/ `failed` |
| `Progress` | `d` | 安装进度 0.0–1.0 |
| `Profile`、`BootBackend` | `s` | 机型、启动后端 |
| `CurrentSlot`、`CurrentVersion` | `s` | 当前 slot（`a`/`b`）与系统版本 |
| `BootState` | `s` | 本次启动 `good`（已确认）/ `trial`（试运行，等待 loong-boot-ok） |
| `OtherVersion`、`OtherState` | `s` | 另一个 slot 的版本（不知道时为空）与状态 `good` / `trial` / `bad` / `missing` |
| `NextBoot` | `s` | 下次启动预计用哪个 slot |
| `PendingVersion` | `s` | 已安装、等待重启的版本 |
| `LastError` | `s` | 上一次失败的原因 |

状态在 loong-sysd 启动时、每次操作之后、以及调用 `Status()` 时重新读取（loong-boot-ok 确认启动后，调用一次 `Status()` 就能看到 `BootState` 变为 `good`）。

| 方法 | 权限 | 说明 |
|---|---|---|
| `Status() → a{sv}` | 所有人 | 重新读取并返回上面的属性，另有 `Slots`（`a(ssss)`：slot、状态、版本、细节） |
| `Inspect(s source) → a{sv}` | root、loong | 只读清单并验签（很快，不读载荷）。返回 `Version`、`Profile`、`Arch`、`BootBackend`、`MinVersion`、`Created`、`Size`（`t`，载荷总字节数）、`Signer`、`Compatible`（`b`），不能安装时 `Error`/`Reason` 是原因 |
| `Apply(s source, b allow_downgrade)` | root、loong | 开始安装，**立即返回**；进度见信号。已有安装在进行时返回 `Busy` |
| `Cancel()` | root、loong | 取消正在进行的安装（目标 slot 保持作废） |
| `Rollback(b force) → s` | root、loong | 下次启动改用另一个 slot（重启后生效），返回那个 slot。另一个 slot 是坏的时需要 `force` |

| 信号 | 说明 |
|---|---|
| `ProgressChanged(s state, d progress)` | 阶段变化时、以及进度每变化 1% 时发一次 |
| `Finished(b success, s error, s message)` | 安装结束。成功时 `error` 为空；失败时是错误名与说明 |

`source` 是绝对路径或 `http(s)://` URL。**非 root 调用者给的路径必须是它自己能读的普通文件**（属主是调用者且可读，或者所有人可读）：loong-sysd 以 root 运行，不能替调用者读它读不到的文件；块设备只有 root 能用。`Apply` 用简单参数而不是 `a{sv}`，因为镜像里只有 dbus-send，它构造不出 `a{sv}`。

错误名（`org.loong.Update1.Error.*`）：

| 名称 | 含义 |
|---|---|
| `AccessDenied` | 调用者无权执行，或读不到给出的文件 |
| `InvalidArgs` | 参数不合法（相对路径等） |
| `InvalidBundle` | 不是合法的更新包（格式、缺文件、清单无效、与清单不符） |
| `BadSignature` | 签名无效，或没有可信的公钥 |
| `HashMismatch` | 载荷的 SHA-256 与清单不符，或写入后回读校验失败 |
| `Incompatible` | 机型、架构、启动后端不符，或不满足 `min_version` |
| `NotNewer` | 版本不比当前新（没有允许降级） |
| `TooLarge` | 目标分区或 ESP 放不下 |
| `Unconfirmed` | 本次启动还没确认，现在不能覆盖另一个 slot |
| `Busy` | 另一个更新操作正在进行 |
| `Cancelled` | 已取消 |
| `NotSupported` | 本机不支持 A/B 更新、找不到分区等 |
| `Io`、`Failed` | 读写错误、其他失败 |

权限有两道检查（与 `org.loong.System1` 相同）：总线策略（写在 [`org.loong.System1.conf`](../../system/rootfs/usr/share/dbus-1/system.d/org.loong.System1.conf) 里——两个服务名属于同一个进程，`send_destination` 匹配接收方占用的任何一个名字，分成两个文件会互相放行），以及 loong-sysd 按调用者 uid 复核。

### 调试

```sh
dbus-send --system --print-reply --dest=org.loong.Update1 /org/loong/Update1 org.loong.Update1.Status
dbus-send --system --print-reply --dest=org.loong.Update1 /org/loong/Update1 \
  org.loong.Update1.Apply string:/data/update-0.1.1-qemu-x86_64.loongup boolean:false
dbus-monitor --system "sender='org.loong.Update1'"
```

## 测试

```sh
tools/dev-chroot.sh run cargo test -p loong-update            # 单元测试：清单、签名、哈希、tar、两种后端（假设备）
tools/dev-chroot.sh run cargo test -p loong-sysd --test update  # 私有 dbus-daemon 上的 Update1
```

`LOONG_UPDATE_ROOT=<目录>` 把 loong-update（以及 loong-sysd 的 Update1）的根目录指向一个假目录树：`proc/cmdline`、`sys/class/block/*/uevent`、`dev/*`（普通文件充当分区）、`etc/loong/update.conf`（`ESP_DIR=/esp` 指定一个目录充当已挂载的 ESP，`QBOOTCTL=` 指定模拟的 qbootctl）。

QEMU 中的端到端验证（升级、确认、坏更新自动回滚、坏签名/坏哈希被拒绝）见 [ADR 0006](../adr/0006-ota-更新包与-loong-update.md#验证)。
