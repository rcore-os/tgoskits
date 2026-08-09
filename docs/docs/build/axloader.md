---
sidebar_position: 13
sidebar_label: "Axloader"
---

# Axloader

`cargo xtask axloader` 是 axbuild 对 `bootloader/axloader`（一个 UEFI bootloader）的构建与 HTTP smoke 测试入口。axloader 本身在裸机/UEFI 环境运行，无法用普通 `cargo test` 验证它的核心能力——通过网络下载内核镜像并加载。axloader 子模块负责用 QEMU + 一段最小 host HTTP 服务把这条链路端到端跑通，确保每次 CI 都能验证 bootloader 真的能从网络取到内核、解析 ELF 并跳转入口。

## 命令结构

```text
cargo xtask axloader <subcommand>
  build   构建 axloader EFI 二进制
  test    运行 axloader 测试套件
    qemu  host 检查 + QEMU HTTP smoke 测试
```

| 子命令 | 参数 | 说明 |
|--------|------|------|
| `build` | `--target <TRIPLE>`（默认 `x86_64-unknown-uefi`）、`--release`/`--debug`（互斥） | 调 `cargo build -p axloader --bin axloader --target ...` |
| `test qemu` | `--target <TRIPLE>`（默认 `x86_64-unknown-uefi`） | 顺序：① `cargo test -p axloader --all-targets` ② `cargo check --target <uefi> --bin axloader` ③ HTTP smoke test |

构建默认 `--release`（`args.release || !args.debug`），因为 smoke 测试要求 release 产物。

## HTTP Smoke Test 流程

这是 axloader 模块最核心的部分，串起了 bootloader 的网络引导能力验证：

```mermaid
flowchart TB
    BUILD["1. cargo build --release<br/>产出 target/<uefi>/release/axloader.efi"]
    FW["2. OvmfFirmware::fetch<br/>Ostool 固定版本、校验与缓存"]
    ESP["3. 组装 ESP<br/>esp/EFI/BOOT/BOOTX64.EFI"]
    KERNEL["4. minimal_x86_64_kernel_elf<br/>手工拼装的 2 字节 jmp . 内核"]
    HTTP["5. SmokeHttpServer::start<br/>监听 0.0.0.0:0 提供 /kernel.elf"]
    QEMU["6. spawn_axloader_qemu<br/>q35 + KVM/host CPU + pflash(OVMF)<br/>+ user net(virtio-net-pci)"]
    SERIAL["7. 监听 QEMU stdio<br/>等待 'AXLOADER READY'"]
    BOOT["8. 写入 AXLOADER BOOT JSON 行<br/>含 kernel_url=http://10.0.2.2:<port>/kernel.elf"]
    FETCH["9. QEMU user-net 访问 host<br/>10.0.2.2 (QEMU_HOST_GATEWAY)"]
    LOAD["10. 等待 'elf_loaded:' 输出"]
    CHECK["11. was_requested() & loaded<br/>否则用全新实例重试一次"]

    BUILD --> FW --> ESP --> KERNEL --> HTTP --> QEMU --> SERIAL --> BOOT --> FETCH --> LOAD --> CHECK
```

关键点：

- **UEFI 固件定位**：`OvmfFirmware::fetch` 复用 Ostool 的固定版本、镜像探测、SHA-256 校验与离线缓存语义。默认缓存根目录由 Ostool 统一确定为 `${TMPDIR}/ostool/ovmf`；需要隔离缓存时可设置 `TGOS_OVMF_DIR`。Axloader 不扫描系统 OVMF 目录，也不维护专用固件环境变量或下载器。
- **虚拟化契约**：x86_64 smoke 显式使用 `-accel kvm -cpu host`，必须在有可用 `/dev/kvm` 的主机上运行。固定的 ostool OVMF 在 QEMU 默认 TCG/`qemu64` CPU 下不会发布本测试需要的 IPv4/HTTP 协议；CI 因此将该任务调度到带 KVM 标签的 runner。
- **QEMU user-net 网关**：QEMU 的 user-mode 网络把 host 映射为 `10.0.2.2`（常量 `QEMU_HOST_GATEWAY`），因此 guest 内的 axloader 通过这个 IP 访问 host 上临时启动的 HTTP 服务，无需配置 bridge/tap。
- **最小内核**：`minimal_x86_64_kernel_elf` 手工拼装一个极简 ELF64——程序头指向 `0x20_0000`，入口指令为 `eb fe`（`jmp .`）。它不需要做任何实际工作，smoke test 只关心 axloader 是否成功下载、解析 ELF 并报告 `elf_loaded:`。
- **协议 JSON**：通过 QEMU 的 stdio 串口向 axloader 发送一行 `AXLOADER BOOT {...}`，字段包括 `protocol_version`、`boot_id`、`kernel_url`、`kernel_size`、`image_format=elf64`、`arch`、`entry_symbol`。axloader 看到 `AXLOADER READY` 提示后才开始读取这行命令。
- **分阶段超时**：每次尝试先给 OVMF 最多 240 秒输出 `AXLOADER READY`；发送 `AXLOADER BOOT` 后重新计时，给内核传输与 ELF 装载 30 秒。这样慢启动不会侵占网络传输窗口。
- **重试与成功判据**：QEMU 提前退出、输出关闭或阶段超时时，会保留退出状态、失败阶段与 transcript，并用新的 ESP、HTTP server 和 QEMU 实例重试一次（最多两次尝试）。成功必须同时满足 ① transcript 出现 `elf_loaded:`；② HTTP server 的 `was_requested()` 为真（即 axloader 真的请求了 `/kernel.elf`）。只满足其中一条仍判定失败，防止假阳性。

## SmokeHttpServer

`SmokeHttpServer` 是一个极简的非阻塞单线程 HTTP/1.1 服务器，只为 smoke test 而存在：

- `TcpListener::bind("0.0.0.0:0")` 随机端口，避免 CI 并发冲突。
- `set_nonblocking(true)` + 10ms 轮询，配合 `AtomicBool` stop 标志优雅退出。
- 仅响应 `GET /kernel.elf`，对每个连接回 `200 OK` + `Content-Length` + 固定 body；同时把 `requested` 标志置位。
- `Drop` 实现里设置 stop 并 join 线程，保证测试结束即释放端口。

## QEMU 启动参数

`x86_64_qemu_args` 构造的命令行（核心字段）：

```bash
qemu-system-x86_64 \
  -m 256M -smp 1 -machine q35 -accel kvm -cpu host \
  -display none -monitor none -serial stdio \
  -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
  -drive if=pflash,format=raw,readonly=on,file=<OVMF> \
  -drive format=raw,if=ide,file=fat:rw:<esp_dir>
```

ESP（EFI System Partition）通过 QEMU 的 `fat:rw:` 内存盘映射提供，省去真实磁盘镜像的构建。`-serial stdio` 让 axloader 的串口输出直接进入 axbuild 进程，配合 `stdin` 注入 `AXLOADER BOOT` 行实现双向通信。网卡使用固定 ostool OVMF 包含驱动的 `virtio-net-pci`；该固件的 E1000 驱动是可选组件，不能作为 smoke 的默认依赖。

## 模块组成

axloader 是单文件实现：

| 代码位置 | 作用 |
|----------|------|
| `scripts/axbuild/src/axloader/mod.rs` | CLI 入口（`ArgsBuild`/`ArgsTest`/`Command`）、构建、HTTP smoke 测试、`SmokeHttpServer`、最小 ELF 构造 |

常量集中在文件头部：

```rust
const AXLOADER_PACKAGE: &str = "axloader";
const AXLOADER_BIN: &str = "axloader";
const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";
const HTTP_SMOKE_BOOT_TIMEOUT: Duration = Duration::from_secs(240);
const HTTP_SMOKE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_SMOKE_MAX_ATTEMPTS: usize = 2;
const QEMU_HOST_GATEWAY: &str = "10.0.2.2";
```

`LoaderSmokeTarget` 是按 target 抽象的测试目标（协议 arch、OVMF arch、EFI 文件名、QEMU 程序、QEMU 参数构造函数、内核 ELF 工厂），目前只实现了 `x86_64-unknown-uefi`。新增架构（如 aarch64 UEFI）时按同样模式扩展 `smoke_target` 即可。

## 用法示例

```bash
# 构建 release EFI 二进制
cargo xtask axloader build --release

# 完整测试：host 单测 + UEFI check + HTTP smoke
cargo xtask axloader test qemu

# 显式指定 target（未来支持更多 UEFI target 时）
cargo xtask axloader test qemu --target x86_64-unknown-uefi
```

CI 中通常直接 `cargo xtask axloader test qemu`。本地运行 x86_64 smoke 需要安装 `qemu-system-x86_64` 并拥有 `/dev/kvm` 访问权限。固件由 Ostool 自动获取并复用 `${TMPDIR}/ostool/ovmf` 缓存；需要使用预先准备的 Ostool 格式缓存时设置 `TGOS_OVMF_DIR=/path/to/cache-root`。
