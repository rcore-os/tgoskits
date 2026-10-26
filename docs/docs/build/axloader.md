---
sidebar_position: 13
sidebar_label: "Axloader"
---

# Axloader

`cargo xtask axloader` 负责构建 x86_64 UEFI 装载器；LoongArch EFI 前端通过 `cargo xtask arceos` 使用 axstd 构建。两条路径都提供设备 HTTP 启动、可选 cmdline/initramfs 和 A/B OTA。axloader 启动后是 TCP4 HTTP 服务端；直连工具与 ostool-server 都调用同一组设备接口。x86 固件路径和 LoongArch axstd 路径都提供串口身份帧，后者在 ArceOS 激活 runtime UART 时通过 axstd console 报告实际波特率和 8N1 参数；没有活动 UART 时继续 fail-closed 并要求显式 `direct` continue。

## 1. 构建入口

本地入口由 `scripts/axbuild/src/axloader/mod.rs` 提供。当前支持 `x86_64-unknown-uefi + OVMF + q35`，产物包含负责选择 A/B 槽的启动器和提供设备接口的装载器。

### 1.1 命令

`build` 构建 EFI 文件，`test qemu` 依次执行宿主测试、UEFI target 检查、release 构建和真实网络场景。

```text
cargo xtask axloader <subcommand>
  build   构建 axloader EFI
  test
    qemu  执行宿主测试、UEFI check 和真实网络启动/OTA
```

常用命令如下；项目验证应从工作区根目录执行。

```bash
cargo xtask axloader build --target x86_64-unknown-uefi --release
cargo xtask axloader test qemu --target x86_64-unknown-uefi
```

### 1.2 产物与布局

`run_loader_build()` 生成两个 EFI 文件。安装后的 ESP 由固定启动器选择装载器槽，普通 OTA 只写非活动槽。

| 产物 | ESP 位置 | 职责 |
| --- | --- | --- |
| `axloader-launcher.efi` | `EFI/BOOT/BOOTX64.EFI` | 校验双状态记录和槽摘要，选择 A/B 槽，处理未确认试运行的回滚 |
| `axloader.efi` | `EFI/AXLOADER/A.EFI` 或 `B.EFI` | 广播设备、提供 HTTP 启动和 OTA 接口、交接目标内核 |

`axloader::ota::State` 保存稳定槽、待试槽、摘要、升级 ID、来源和结果；`axloader::ota::OtaDisk` 负责 `STATE0.BIN`、`STATE1.BIN` 与槽文件的 Flush 和读回校验。两份状态都损坏或没有可信槽时，启动器停止并要求外部介质恢复。

### 1.3 LoongArch EFI 构建

LoongArch 目标使用 `loongarch64-unknown-none-softfloat` 和 `arceos-efi`，由
`bootloader/axloader/build-loongarch64-unknown-none-softfloat.toml` 选择
axstd、FAT/NVMe 和 `efi-handoff` 能力，再由
`bootloader/axloader/qemu-loongarch64.toml` 配置 `virt`、virtio-net、NVMe/FAT
磁盘、`uefi = true` 和 `to_bin = true`。`axstd_loader::run()` 负责单任务轮询
发现和 HTTP 路由；`TargetSpec::LOONGARCH64_EFI` 固定 ELF machine
`EM_LOONGARCH` 与 `efi_pe_entry`。someboot 公共 `boot_resources` 管理类型化资源，
`handoff::HandoffSpec` 和 `PreparedHandoff` 管理两条平台路径的交接事务；x86_64
继续使用 EFI LoadOptions 与 Linux EFI LoadFile2。

`boot-resources` 可以独立使用；`efi-handoff = ["efi", "boot-resources"]` 开启生产端，
普通 EFI 消费始终跟随构建脚本的 `cfg(efi)`。原生 UEFI target 只编译 someboot
公共能力与固件适配，不带入内核入口、分页或 SMP。LoongArch 的 v4 配置表承载资源
快照、镜像、cmdline/initramfs 和应用元数据，不包含固定三池或 OTA 槽位字段；
GUID 为 `7b9fa931-a25b-d043-b13c-63435428c473`。v2/v3 已拒绝，迁移须同时重建
生产者、目标与 A/B 槽。完整边界见[设计文档](https://github.com/rcore-os/tgoskits/blob/dev/docs/design/someboot-boot-resources.md)。

`axstd_loader::run()` 在启动及每次连接处理后发送 UDP 公告，阻塞等待连接期间
不周期重发。依赖公告发现的服务端须先就绪；服务端后启动或启动广播丢失时，
使用已知设备 IP 直接访问 HTTP 接口。

两前端使用 `http_control::Route` 共享 epoch/trial 准入，三种 GET 状态查询无需
epoch，单值控制头重复时返回 `400`。`elf::ElfImage` 统一解析和校验可执行 EFI
入口，具体装载地址、内存申请和缓存维护由后端负责。LoongArch 在最终跳转前
通过 `ax-runtime::handoff::enter()` 同步文件系统、停止块设备与网络 DMA、join
网络任务、关闭串口及本核中断；当前限定一个运行处理器。停稳失败或保持调用方
执行上下文的 EFI 目标返回时复位；已接管栈、MMU 或处理器状态的内核不得返回。

```bash
cargo xtask arceos build \
  --package axloader \
  --config bootloader/axloader/build-loongarch64-unknown-none-softfloat.toml
cargo xtask arceos qemu \
  --package axloader \
  --config bootloader/axloader/build-loongarch64-unknown-none-softfloat.toml \
  --qemu-config bootloader/axloader/qemu-loongarch64.toml
```

要验证目标 ELF 的正向交接，先构建独立的 ArceOS EFI 应用并打包宿主归档：

```bash
cargo xtask image pack-initramfs test-suit/host-initramfs \
  target/axbuild/host-initramfs/loong-host.cpio
cargo xtask arceos build \
  --package arceos-helloworld \
  --config bootloader/axloader/build-loongarch64-helloworld-efi.toml
```

该配置启用 `loong-efi-smoke`，使用 `loongarch64-unknown-none-softfloat`、
`ax-std/efi`、cmdline/initramfs smoke 特性，生成
`target/loongarch64-unknown-linux-musl/release/arceos-helloworld`。目标应用的
输出用于确认 EFI 入口已经完成 cmdline 和宿主 initramfs 交接。

完整 LoongArch 回归由以下入口统一构建并运行，不需要手动保留后台 QEMU：

```bash
python3 bootloader/axloader/scripts/loong-qemu-smoke.py
```

脚本先执行 `boot-resources` 独立资源应用，再根据 `qemu-loongarch64-http.toml`
创建独占 FAT、QMP socket 和空闲宿主端口，运行正常 HTTP/OTA/ELF 与 FAT 写满两组
场景。loader 构建配置中的 `to_bin = true` 让 `arceos build` 在准备 FAT 前生成
EFI `.bin`。失败时输出 QEMU 日志并停止自己创建的进程组。可加
`--output-dir /tmp/loong-efi` 保留本次运行证据。需要 QEMU LoongArch、dosfstools、
mtools 和 `python3`。

LoongArch 的持久文件位于 FAT 卷的 `axloader/` 目录：`A.EFI`、`B.EFI`、
`STATE0.BIN` 和 `STATE1.BIN`。`LaOta::prepare_bootstrap()` 在进入待试槽前提交
`attempted`；`ota::handoff` 编解码应用自己的槽位和代次，someboot 仅传递有界
元数据。确认由 `LaOta::confirm()` 按升级 ID 和来源提交。没有可信启动熵时先提交
持久代次，再派生 `boot_epoch`；继承入口复用父 loader 已提交的代次。

axloader 的静态计划声明 32 MiB 镜像区（`0x200000`）、256 MiB 归档区、32 MiB
EFI 槽执行区和 32 MiB 原文件区。首次启动在退出服务前通过
`InitialContents::FirmwareBootImage` 保存最初固件启动文件，再校验、Flush 并读回
`A.EFI`，最后创建状态记录；不使用已修改的运行镜像字节。该配置要求最初从可读的
EFI 文件系统启动，源文件不超过 32 MiB，来源不可用或必需资源申请失败时停止。
仅接受 `ImageBase = 0`、无 base relocation directory 的 someboot LoongArch PE。
继承阶段保留同一份固件来源文件，当前槽身份以持久状态为准，缺失状态时拒绝建档。
非 EFI LoongArch、UHI FDT handoff 和 AArch64 EFI 镜像生成不在本阶段。

## 2. 设备控制

v6 把控制面放在设备上。`network::NetworkInterface` 要求同一 UEFI 控制器同时提供 SNP、IP4、UDP4 和 TCP4；`network::Announcer` 向 UDP `2998` 单向广播带 `serial_id` 与 `serial_ready` 的 `LoaderAnnouncement`，`direct::Listener` 在 TCP `2999` 接受请求。设备不再发现、轮询或下载 ostool-server 的资源。

### 2.1 发现与代次

每次固件启动都会生成新的 `boot_epoch`。调用方先读取 `GET /api/v1/status`，随后在修改请求中携带相同的 `X-Boot-Epoch`；旧代次返回 `409`。广播只用于发现，有设备地址的直连调用方不依赖 ostool-server。

```mermaid
sequenceDiagram
    participant L as axloader/OVMF
    participant C as 直连工具或 ostool-server

    L-->>C: UDP 2998 LoaderAnnouncement（可选转发）
    C->>L: GET /api/v1/status
    L-->>C: LoaderDeviceStatus、boot_epoch、serial_id 与参数
    C->>L: POST /api/v1/serial/continue（bound 或 direct）
    C->>L: HTTP 修改请求与 X-Boot-Epoch
```

本地服务端联调只把测试环境中的设备地址映射到 QEMU `hostfwd` 端口。ostool-server 随后通过真实 HTTP 连接操作客户机中的 axloader，测试夹具不替设备生成响应。

### 2.2 启动事务

`boot_server::BootServer` 保存单个内存事务，并直接使用 `httpboot_protocol::DeviceBootJob`、`DeviceBootImage` 和 `DeviceBootStatus`。同一启动 ID 与相同清单可以重试；冲突清单或并发事务返回 `409`。cmdline 和 initramfs 相互独立，均可省略。

| 接口 | 行为 |
| --- | --- |
| `POST /api/v1/boot/jobs` | 提交启动 ID、x86_64 ELF64、文件长度和 SHA-256，以及可选 cmdline/initramfs |
| `GET /api/v1/boot/jobs/{id}` | 查询文件接收状态和阶段 |
| `PUT /api/v1/boot/jobs/{id}/kernel` | 定长上传内核并核对 `X-Image-Sha256` |
| `PUT /api/v1/boot/jobs/{id}/initramfs` | 上传清单声明的可选归档 |
| `POST /api/v1/serial/continue` | 携带 `X-Boot-Epoch`、`serial_id`、`binding_id` 和 `bound/direct`；相同请求幂等，`bound` 需要当前串口身份就绪 |
| `DELETE /api/v1/serial/bindings/{binding_id}` | 撤销当前启动的绑定令牌并恢复身份帧 |
| `DELETE /api/v1/boot/jobs/{id}` | 取消尚未执行的事务 |
| `POST /api/v1/boot/jobs/{id}/start` | 完成 ELF 与载荷检查，核对 `X-Serial-Binding`，回复 `202`，关闭网络对象后交接 |

x86_64 v6 只接受 `__x86_64_efi_pe_entry`。`someboot::handoff::PreparedHandoff` 在内部将 cmdline 编码为带 NUL 的 UCS-2 EFI LoadOptions，并为可选 initramfs 注册 Linux EFI `EFI_LOAD_FILE2_PROTOCOL` 提供者。启动文件单个上限 256 MiB，请求头上限 4 KiB，只接受定长 body。上传、状态查询和 OTA 不受串口门禁限制；没有隧道需求的调用方必须显式使用 `direct` continue。

LoongArch 请求使用 `BootArch::Loongarch64`、`ImageFormat::Elf64` 和
`efi_pe_entry`。`axstd_loader::LoadedElf` 拒绝错误 machine、入口、重叠
`PT_LOAD` 或与当前 loader 重叠的装载区域；`PublishedPayload` 通过
共同的 `HandoffSpec`/`PreparedHandoff` 交接 cmdline/initramfs。目标 EFI
镜像由 `LoadedPe` 校验 LoongArch PE/COFF machine 并从独立槽位内存池装载。

v6 设备必须与支持 `POST /api/v1/serial/continue`、`X-Serial-Binding` 的 ostool-server
配套，板卡 ESP/U 盘中的 `axloader.efi` 也要更新到同一版本。启动返回
`409 serial_binding_required` 时，先检查服务端与 loader 是否配套，再检查当前
`serial_id` 是否已完成绑定；无需串口隧道时使用显式 `direct` continue。

### 2.3 OTA 事务

OTA 实现集中在 `bootloader/axloader/src/ota/`。`state.rs` 维护纯状态机与记录编解码，`disk.rs` 独占 ESP 文件访问，`runtime.rs` 的 `OtaController` 负责当前运行槽、上传失败原因和确认操作。该分层让宿主测试可以验证状态机，同时把 UEFI I/O 限制在目标专用模块。

| 接口 | 行为 |
| --- | --- |
| `GET /api/v1/ota/status` | 返回运行槽、稳定槽、待试 ID、来源和最近结果 |
| `PUT /api/v1/ota/image` | 流式写非活动槽，核对长度、SHA-256、PE/COFF 和 UEFI LoadImage 后重启 |
| `POST /api/v1/ota/confirm` | 仅由匹配升级 ID 与来源的待试槽提交为稳定槽 |

EFI 镜像上限 32 MiB。待试槽确认前拒绝内核启动；启动失败、掉电或复位后，启动器清除未确认槽的可信摘要并回到稳定槽。替换 `BOOTX64.EFI` 仍有断电窗口，安装时必须准备外部恢复介质；迁移模式另保留旧装载器备份。

## 3. 安装与本地验证

安装脚本支持两种入口：

```bash
# 全新安装：不要求 ESP 中存在旧 BOOTX64.EFI
./bootloader/axloader/scripts/build-install-efi.sh \
  --fresh --device /dev/sdb1

# 迁移已有装载器：把旧 BOOTX64.EFI 保存为 A
./bootloader/axloader/scripts/build-install-efi.sh \
  --device /dev/sdb1
```

`--fresh` 在 ESP 上创建 `EFI/AXLOADER/A.EFI`、`B.EFI`、两份状态记录和
`EFI/BOOT/BOOTX64.EFI` launcher。A、B 初始是同一份新装载器，A 稳定启动，
首次启动不需要试运行确认。ESP 中若已有 `BOOTX64.EFI`，脚本只保存为
`EFI/AXLOADER/BOOTX64.PREVIOUS.EFI`，不把它纳入自动回滚。迁移模式要求旧
`BOOTX64.EFI`，将其保存为 `BOOTX64.ORIGINAL.EFI`，并让 B 首次作为待试槽。
两种模式都拒绝覆盖已有 `EFI/AXLOADER` 文件；首次替换 launcher 仍须准备外部
恢复介质。

QEMU 验证使用真实 FAT 磁盘、OVMF VARS 和 SLiRP `hostfwd`。`scripts/axbuild/src/axloader/ota_qemu.rs` 属于宿主测试编排层，负责创建 ESP、启动 QEMU、调用设备 HTTP 接口并跨多次启动复用同一磁盘。

### 3.1 执行链

`test qemu` 的每一步都有不同责任：宿主测试验证纯状态和解析，UEFI check 验证目标组合，QEMU 场景验证固件网络、持久磁盘和真实内核交接。

1. 执行 `cargo test -p axloader --all-targets`。
2. 检查 `axloader` 的 `x86_64-unknown-uefi` 目标。
3. release 构建启动器、装载器及 ArceOS、StarryOS、AxVisor 的真实 EFI 入口 ELF；由现有镜像存储和 AxVisor 归档流水线准备 BusyBox、Linux 及客体配置。
4. 把启动器、A/B 槽和双状态记录写入真实 FAT 镜像。
5. 用 `hostfwd` 调用客户机 TCP `2999`，先读取状态并完成 `bound/direct` continue，再覆盖直连启动、OTA、重启确认和回滚。
6. 在五种可选载荷组合中观察启动后的内核输出。
7. 上传 StarryOS 和 AxVisor 的 ELF 与归档，分别运行 StarryOS 用户态及 AxVisor 的 Linux 客体用户态。

该流程不会安装 systemd 服务，不会修改 runner、板卡配置或实体设备。宿主需要 `qemu-system-x86_64`、KVM、`mkfs.vfat`、`mcopy`、`mmd` 和 `python3`。AxVisor 用例使用 2 GiB、2 vCPU 和 `host,-la57`，要求宿主暴露嵌套 VMX/SVM。

### 3.2 成功判据

测试必须到达目标内核，`ready_to_handoff` 只表示 axloader 已准备交接。四种组合分别验证无附加载荷、仅 cmdline、仅 initramfs 和两者都有。

- 基础场景输出 `Hello, world!`。
- cmdline 场景输出精确的 `HOST_CMDLINE`。
- initramfs 场景输出 `HOST_INITRAMFS_PASSED`。
- initramfs 回退场景验证 ESP 上的 Linux EFI initrd provider。
- StarryOS 必须运行归档中的 shell 脚本，检查 cmdline 参数并完成文件写读，输出完整行 `STARRY_AXLOADER_USERSPACE_PASSED`。
- AxVisor 必须消费宿主归档中的 builtin 配置和 Linux 镜像，按宿主参数 `axvisor.vm_start=1` 启动客体；测试等待管理 shell 后执行 `vm console 1`，客体脚本完成同样检查后输出完整行 `AXVISOR_GUEST_AXLOADER_USERSPACE_PASSED`。
- OTA 场景跨 QEMU 启动验证坏摘要、短请求、未确认复位回滚、确认持久化和再次启动。

`ota_qemu::payloads::OsPayloads` 只负责准备 OS 载荷，复用 `boot_smoke()` 的真实 HTTP 上传、校验和串口门禁。构建在 `AXBUILD_NO_SNAPSHOT=1` 子进程中调用现有 `cargo xtask starry/axvisor build`，前后逐字节核对保存的命令快照，保留用户默认选择；不修改多线程父进程的全局环境。成功行不出现在 cmdline 原文中，命令行回显不能替代实际执行。每次运行在 `target/axbuild/axloader/qemu/x86-*` 保留 ESP、VARS、历次串口和 stderr；提前退出或超时也保留。测试应根据具体失败阶段报告 HTTP、网络、磁盘或内核交接错误，不能用重试掩盖确定性失败。

LoongArch CI 使用 `scripts/loong-qemu-smoke.py`。资源用例不启用网络、OTA 或
交接生产端，以 `BOOT_RESOURCES_PASSED` 和 `Hello, world!` 作为实际执行证据。
HTTP 用例验证 manifest 幂等、serial continue、坏 ELF/OTA 拒绝、有效 PE 槽位写入，
未确认 trial 经 QMP 复位回滚到原稳定摘要、旧确认拒绝，以及再次 trial/confirm 和再次复位后的稳定摘要。
独立 `arceos-helloworld` EFI ELF 与宿主归档通过 HTTP 上传，目标必须输出
`HOST_CMDLINE: axloader-positive=1`、`HOST_INITRAMFS_PASSED` 和 `Hello, world!`。
写满的独占 FAT 则验证 `409 ota_storage_failed` 且稳定状态未提交；错误镜像仍为
`422 image_digest_or_efi_validation_failed`。`ready_to_handoff` 单独出现只表示准备交接。

普通 `qemu-loongarch64.toml` 仅等待 `network_ready: arch=loongarch64` 和
`loader_server: port=2999`。LoongArch 在活动 runtime UART 未绑定时输出身份帧，
状态与 UDP 公告报告实际波特率和 8N1 参数；无活动 UART 时报告 `ready=false`，
客户端须显式 `direct` continue。绑定后暂停身份帧，撤销绑定后恢复。

## 4. 故障定位

诊断从设备状态和稳定日志标记开始。广播失败与 TCP 监听失败的影响不同，持久状态损坏也不能按普通网络错误处理。

### 4.1 网络路径

下表把常见日志或 HTTP 结果映射到最先检查的边界。

| 现象 | 优先检查 |
| --- | --- |
| `loader_tcp4_unavailable` | 同一网卡是否同时发布 SNP、IP4、UDP4、TCP4 service binding |
| `loader_tcp4_listen_failed` | 固件能否在 TCP `2999` 建立被动监听，端口是否已占用 |
| `loader_broadcast_error` | UDP `2998` 的广播地址和载荷大小；公告是可选发现路径，有设备 IP 时仍可直连 |
| `serial_binding_required` | 先读取当前 `serial_id`，通过 `POST /api/v1/serial/continue` 完成 `bound` 或显式 `direct` continue，再在启动请求中携带返回的绑定令牌 |
| `serial` 为 `ready=false` | 检查 UEFI `ConOut` 或 axstd EFI console 是否可用，再读取新的 `serial_id` 和 `boot_epoch` |
| `409 stale_boot_epoch` | 重新读取 `/api/v1/status` 并使用新的启动代次 |
| 上传被拒绝 | `Content-Length`、`X-Image-Sha256`、清单长度和当前事务 ID |

设备回复准备交接后，`direct::Listener`、TCP 子句柄、事件和 UDP 广播对象必须先析构。退出 Boot Services 后不能继续调用固件网络或控制台服务。

### 4.2 启动与恢复

`ready_to_handoff` 后失败时检查 ELF 入口、EFI LoadOptions 和 Linux EFI initrd provider。OTA 复位循环则先读取两份状态记录的代次与校验和，再核对当前槽文件摘要。

| 现象 | 优先检查 |
| --- | --- |
| cmdline 缺失 | 清单字段、`PreparedLoadOptions`、someboot 的 EFI image handle 与命令行优先级 |
| initramfs 缺失 | 清单是否声明归档、上传状态、`LoadFile2` 提供者是否在 ExitBootServices 前读取 |
| 待试槽重复回滚 | 升级 ID、`OtaSource`、运行摘要和确认请求是否匹配 |
| 两份状态均无效 | 停止自动启动，使用备份的原装载器和外部介质恢复 |

当前 OTA 只用 SHA-256 检查传输一致性，适用于可信隔离实验网。MAC 绑定不构成认证；以后引入内核验签时，应先让两槽执行同一验签策略并验证拒绝路径。


LoongArch 交接的资源容量与存活对象分开管理。等待下载的 axloader 继续保有声明容量；
普通目标没有资源计划时，只保留当前镜像、未消费归档及显式存活分配。旧镜像、栈、
页表和逐核区在目标安装自身执行环境并绑定 CPU-local 后才回收，随后才初始化普通分配器。
`PreparedHandoff::seal` 在运行时停稳后的无分配窗口发布最终快照，生成可进入的 `SealedHandoff`。
initramfs 解包借用结束后通过唯一所有者归还完整页；冻结的平台 map 不再修改。
机制、内存分类与失败边界见[启动内存回收设计](https://github.com/rcore-os/tgoskits/blob/dev/docs/design/someboot-boot-resources.md#4-启动内存回收)。
