# ADR 0006：OTA 更新包格式与 `loong-update`

- **状态**：已接受（dm-verity 部分为提议，尚未实现）
- **日期**：2026-10
- **相关**：[ADR 0004](0004-启动与-ota-ab.md)（A/B 布局与两种启动后端）、[loong-update 接口](../api/loong-update.md)

## 背景

[ADR 0004](0004-启动与-ota-ab.md) 定下了只读 erofs 根、A/B 两个 slot、`uefi-sdboot` 与 `android-abl` 两种启动后端，以及“整镜像写入另一个 slot、靠启动计数自动回滚”的 OTA 思路。M5 要把它落地，需要决定：

1. 更新包长什么样、怎么签名，设备怎样只信任我们签的包；
2. 写入非活动 slot 的顺序，保证断电、坏包、坏系统都不会让设备变砖；
3. “这次启动成功了”由谁、按什么标准判断；
4. 系统界面（loong-shell、设置应用）怎样触发和观察更新。

约束：系统要小（不引入 RAUC/SWUpdate/OSTree，见 ADR 0004），数据量是整个根文件系统（目前约 210 MiB），设备内存不能被整包占用；构建在开发 chroot 和 CI 里进行，不依赖 loop 设备或特权操作。

## 决策

### 更新包：ustar 归档 + 签名清单

- 一个更新包是一个普通的 **ustar 归档**：`manifest.toml`、`manifest.sig`、载荷（`rootfs.img` 与每个 slot 的启动镜像）。普通工具就能查看（`tar -tvf`），也可以直接放在块设备上（QEMU 测试时作为额外的一块盘）。
- **清单（TOML）**：`format`、`version`、`profile`、`arch`、`boot_backend`、可选的 `min_version`，以及每个载荷的种类、slot、文件名、大小、SHA-256。
- **签名：ed25519，签清单的原始字节**；清单里有载荷的哈希，所以一次验签就覆盖整个包。设备信任 `/usr/share/loong/update-keys/*.pub`（只读根分区上，随系统更新，可以借此轮换密钥）。签名不对时清单根本不进入 TOML 解析器。
- 密钥用标准 PEM（PKCS#8 / SubjectPublicKeyInfo），与 OpenSSL 逐字节兼容：开发密钥由 mkimage 用 `openssl genpkey` 生成在 `build/keys/`（不进 git）；正式发布只把公钥交给构建（`LOONG_UPDATE_PUBKEY`），私钥可以留在离线机器上，用 `loong-update pack` 或 `openssl pkeyutl -rawin` 签名。
- 启动镜像**每个 slot 一份**：UKI 与 boot.img 的 cmdline 里写着 `loong.slot=<slot>`（UKI 的 cmdline 以后还要带 dm-verity 根哈希，见下文），不能在设备上改。多出的一个约 13 MiB，相对 210 MiB 的根文件系统可以接受。

### 写入顺序：先校验、再作废、再写、回读、最后切换

1. 本次启动必须**已确认**；试运行中的系统不能覆盖另一个 slot（那是唯一的后备）。
2. 验签、检查机型/架构/后端/`min_version`/版本（默认拒绝降级）、目标分区放得下。
3. 来源能读两遍（文件、块设备）时，**先完整读一遍、校验所有哈希，什么都不写**；只能读一遍的下载流跳过这一步。
4. **作废目标 slot**（uefi-sdboot 删除它的 UKI；android-abl `qbootctl -u`），然后写入，写的同时再算一遍哈希。
5. `fsync`、丢掉页缓存、**回读校验**。
6. **切换**：uefi-sdboot 把临时文件 `loong-<slot>.efi.new` 改名为 `loong-<slot>+3.efi`；android-abl `qbootctl -s <slot>`。

任何一步失败，目标 slot 都处于作废状态，引导程序不会启动半个系统；当前 slot 从头到尾不被触碰。所有数据流式处理，只经过 1 MiB 的缓冲区。

### uefi-sdboot：利用 systemd-boot 的排序与计数

systemd-boot 给 `EFI/Linux/*.efi` 排序时，计数用完的 UKI 排最后，其余按 `.osrel` 的 `IMAGE_VERSION` 从高到低。新 UKI 版本更高，所以下次启动排在最前；连续 3 次没被确认就排到最后，自动回到旧 slot。不需要写 EFI 变量，也不需要改 `loader.conf`。

- **降级**（`--allow-downgrade`）时新 UKI 不会排在前面，于是同时把当前 slot 的 UKI 标成坏的（`+0`）：新 slot 先被尝试；它也失败时两个都是坏的，systemd-boot 按版本选回当前 slot。
- **手动回滚**：另一个 slot 重新获得 3 次尝试，当前 slot 标成坏的。
- ESP 平时不挂载，loong-update 用到时临时挂载（每次一个独立挂载点，loong-sysd 里状态查询和安装可能同时进行）。

### 启动确认：系统界面真的跑起来了才算

`loong-boot-ok` 原来在 `loong-session` 启动后立即确认——supervise-daemon 拉起进程就算“启动成功”，界面一启动就崩溃也会被确认，回滚形同虚设。现在：

- 只在试运行的启动中工作；等 `loong-shell` **连续运行 15 秒**（进程号变了就重新计时），再 `loong-update mark-good`。
- 180 秒内没等到就**重启**，让引导程序消耗一次尝试次数。
- initramfs 挂不上根分区时不再停在救援 shell（平板上没人能操作），10 秒后重启；内核命令行加 `panic=10`。没有这些，坏系统会一直停在那里，启动计数永远用不完。

以后 loong-shell 可以在第一帧显示后通知 loong-boot-ok（例如写一个就绪文件），代替“进程连续运行 15 秒”这个近似判断。

### D-Bus：loong-sysd 链接同一个库

loong-sysd 另外占用 `org.loong.Update1`，直接链接 `loong-update` 库（命令行与 D-Bus 走同一套代码），安装在单独的线程里进行，进度用信号报告。权限与 `org.loong.System1` 一致（root 与 loong），另外检查非 root 调用者能否读取它给出的文件。两个服务名的总线策略必须写在同一个文件里（`send_destination` 匹配接收方占用的任何名字）。

## 备选方案及不选原因

| 方案 | 不选原因 |
|---|---|
| **自定义二进制容器**（头 + 偏移表） | 同样简单，但 tar 可以用现成工具查看和调试，流式读取也一样容易 |
| **签整个包**（例如对整包做 detached 签名） | 要读完整个包才能验签，坏包只能在写入之后才发现；签清单则读到第 2 个成员就能拒绝 |
| **minisign/signify 格式** | 需要额外的格式与工具；PEM + ed25519 与 OpenSSL 互通，离线签名、HSM 都方便 |
| **只用一个 UKI，在设备上改 cmdline** | 改了 cmdline 就破坏了（以后的）UKI 签名与 dm-verity 根哈希的绑定 |
| **用 `LoaderEntryOneShot` 选择下次启动的 UKI** | 依赖 systemd-boot 对带计数文件名的 entry id 的处理细节；按版本排序 + 文件名计数是 systemd 官方的 A/B 做法，`mark-good` 也与 `systemd-bless-boot` 一致 |
| **loong-sysd 调用命令行** | 进程隔离更好，但进度要解析文本；链接库更简单，类型安全。代价是 loong-sysd 大了约 0.5 MiB（见“后果”） |
| **下载到 /data 再安装** | /data 可能放不下整包（nabu 的 userdata 很大，但 QEMU 镜像只有 512 MiB）；流式写入不需要额外空间，作废目标 slot 保证了安全 |
| **调整 super 里逻辑分区的大小（android-abl）** | 需要实现 LP 元数据写入，风险高；mkimage 给每个 slot 留了 50% 余量，放不下时整包刷机 |

## dm-verity（提议，尚未实现）

根文件系统已经只读，下一步是让它**不可篡改**：

1. **构建**：`veritysetup format root.erofs root.hashtree`（cryptsetup 的 veritysetup，开发 chroot 需要加 `cryptsetup` 包）生成哈希树与根哈希；哈希树追加在 erofs 镜像之后（`--hash-offset` = 数据大小，按 4 KiB 对齐），作为同一个 `rootfs` 载荷写入根分区。
2. **根哈希进 cmdline**：`loong.verity=<根哈希> loong.verity_data_blocks=<块数>` 写进 UKI 的 cmdline（UKI 以后用 Secure Boot 签名，根哈希也就被签名保护）；android-abl 写进 boot.img 的 cmdline（由 AVB 的 vbmeta 保护，前提是不刷关闭校验的 vbmeta）。
3. **initramfs**：已经带 `dmsetup.static`（动态分区要用）。加载 `dm_mod`、`dm_verity`，用 `dmsetup create --readonly loong-root --table "0 <扇区数> verity 1 <设备> <设备> 4096 4096 <数据块数> <哈希起始块> sha256 <根哈希> <盐>"` 建立映射，挂载 `/dev/mapper/loong-root`。校验失败的块读取会返回 I/O 错误；加 `restart_on_corruption` 让内核在发现篡改时重启，于是坏 slot 同样由启动计数回滚。
4. **更新**：清单不变（哈希树就在 rootfs 载荷里）；`loong-update` 的回读校验覆盖哈希树。
5. **验证**：QEMU 中改动根分区的一个字节，确认读到该块时报错/重启并回滚到另一个 slot。

nabu 目前的内核没有 erofs，也要确认 `CONFIG_DM_VERITY`；换内核时一起处理。

## 影响 / 后果

**正面**

- 坏签名、坏哈希、不匹配的包在写入任何东西之前就被拒绝；写入过程中的任何失败（包括断电）都只影响已作废的目标 slot。
- 新系统必须让系统界面真正跑起来才会被确认；否则 3 次后自动回到旧系统，不需要用户操作。
- 命令行、D-Bus、loong-boot-ok 共用一个库；两种启动后端的差异集中在 `backend/` 里。

**负面与需要承担的工作**

- 每个更新包是整个根文件系统（约 240 MiB）；差分更新以后再说（例如 erofs 块级差分或 zstd 字典压缩）。
- `loong-update` 约 1 MiB，loong-sysd 链接它之后大了约 0.5 MiB（TOML、ed25519、SHA-256）；M6 瘦身时可以考虑去掉 TOML 依赖或让 loong-sysd 改为调用命令行。
- **android-abl 后端没有在真机上验证**：qbootctl 的输出按其源码解析、用模拟脚本测试；ABL 的重试次数、`qbootctl -s` 是否重置“成功”标志等行为需要在 nabu 上确认（ADR 0004 已指出）。
- 启动确认用“`loong-shell` 连续运行 15 秒”近似“系统界面正常”，以后应改为 loong-shell 主动报告就绪。

## 验证

在 QEMU x86_64（TCG）中完成（`tools/qemu.py` 的 `--disk` 把更新包作为额外的只读磁盘挂进虚拟机）：

1. 构建 0.1.0 镜像，以及 0.1.1 与“坏的”0.1.2 更新包（0.1.2 的 loong-shell 一启动就退出）；另外从 0.1.1 包制作坏签名（改了清单里的版本号）与坏哈希（根文件系统载荷里翻转一个字节）的包。
2. 0.1.0：坏签名包 → `BadSignature`，坏哈希包 → `HashMismatch`（第一遍校验时发现）；前后 `sha256sum /dev/vda3`（root_b）一致，ESP 不变。
3. `loong-update apply /dev/vdb`（0.1.1，约 11 秒）→ 重启 → `loong.slot=b`，`/etc/os-release` 为 0.1.1，loong-boot-ok 确认后 `loong-b+2-1.efi` → `loong-b.efi`。
4. 通过 D-Bus（`org.loong.Update1.Apply`，dbus-send）安装 0.1.2：105 个 `ProgressChanged` 信号，`Finished(true, "", …)`，`NextBoot=a` → 重启 → slot a 连续 3 次试运行，每次 loong-boot-ok 报告“系统界面没有正常启动”并重启 → 第 4 次 systemd-boot 自动回到 slot b（0.1.1），slot a 的 UKI 为 `loong-a+0-3.efi`（坏的），`loong-update rollback` 拒绝回滚到它（`--force` 才行）。
5. 重装（`--allow-downgrade`，同版本 0.1.1 写入 slot a）：slot b 的 UKI 被标为 `loong-b+0.efi`，systemd-boot 确实先启动 `loong-a+3.efi`，确认后为 `loong-a.efi`——降级时“把当前 slot 标成坏的”这一做法在真实的 systemd-boot 260 上成立。
