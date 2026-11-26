# TianHong MiniPC D3 Linux 板卡测试

本目录维护 TianHong MiniPC D3 的 AxVisor 实体板卡 Linux guest 用例。用例只有在 D3 上完成 AxVisor 启动、进入客户机控制台并输出 `test pass!` 才能通过；用例发现、主机归档构建或 QEMU 启动装配都不能替代实体板卡执行。

## 1. 平台身份与范围

D3 是独立的 AMD 实体板卡，不复用 ASUS NUC15CRH 的 VMX 身份。仓库用例使用 `board/tianhong-minipc-d3/linux`，板卡服务按 `TianHong-minipc-D3` 类型分配实体机。

### 1.1 硬件与控制面

控制面资源、CPU 虚拟化能力和宿主串口是不同契约，排障时必须分别核对。

| 项目 | 当前事实 | 运行约束 |
| --- | --- | --- |
| 控制面 `board_id` | `TianHong-minipc-D3-1` | 由板卡服务登记和分配，不写入 guest 配置 |
| 控制面 `board_type` | `TianHong-minipc-D3` | 与 [board-tianhong-minipc-d3-linux.toml](linux/board-tianhong-minipc-d3-linux.toml) 的 `board_type` 一致 |
| Host MAC | `00:e0:4d:05:1a:c3` | 由板卡服务控制面维护，不在 AxVisor suite 中重复配置 |
| 虚拟化能力 | AMD SVM/NPT，由运行时 CPUID 选择 | BIOS 应允许 SVM/NPT；构建不选择 `svm` 或 `vmx` Cargo feature |
| Host UART | `/dev/ttyUSB1`，CP2102 | 仅表示当前主机线路，不证明固件暴露 UEFI `SerialIo` |

`.github/ci/checks/axvisor.toml` 中的 D3 检查使用实体板卡 profile `board`，也就是 `self-hosted`、`linux`、`board` runner 标签。仅提供 AMD KVM 的 `kvm-amd` runner 执行的是 QEMU 嵌套 SVM，不是 D3 实体板卡。

### 1.2 串口与 CPUID 边界

ostool HTTP Boot 的 `serial:null` 是正常控制面状态；串口自动绑定来自 axloader 暴露 `SerialIo` 后发送的 `AXLOADER-SERIAL/1 <serial_id>` 身份帧，不能通过填写 `board.serial` 强行绕过。D3 实机已完成该身份绑定、HTTP Boot 下发，并在不含 AML 的配置下进入 AxVisor 管理 shell；这仍不代表 SVM 或客户机启动成功。

客户机命令行中的 `ttyS0` 是 AxVisor 虚拟串口，与宿主 CP2102 的 `/dev/ttyUSB1` 不是同一设备。已完成的 SerialIo 身份绑定也不能替代真实 UART 资源证明，不得根据 `/dev/ttyUSB1` 推导 UART 内存映射输入输出地址，也不得把 `0x3f8` 或其他 COM1 地址硬编码为 D3 宿主串口。

## 2. 测试配置

该 group 只提供一个 Linux guest 启动与命令检查能力。主机和 guest 资产由构建配置、guest 配置和 CI 镜像准备步骤共同确定，运行时固定选择 VM ID 1。

### 2.1 用例与客户机

板卡运行配置通过 `axvisor.vm_start=1` 启动 VM 1，在 AxVisor 管理 shell 执行 `vm console 1`，随后等待客户机根文件系统输出 `test pass!`。panic、kernel panic、登录失败和权限拒绝继续作为失败信号。

| 文件 | 责任 |
| --- | --- |
| [board-tianhong-minipc-d3-linux.toml](linux/board-tianhong-minipc-d3-linux.toml) | 声明 `board_type`、宿主启动参数、串口检查步骤和 case ID |
| [build-x86_64-unknown-none.toml](linux/build-x86_64-unknown-none.toml) | 构建 x86_64 AxVisor 宿主、选择 D3 guest，并使用不含 AML 的 ACPI 表消费模式 |
| [guest-linux.toml](linux/guest-linux.toml) | 定义 VM 1、Linux 内核、BusyBox initramfs、内存和虚拟控制台命令 |

guest 名称和各资产路径均使用 D3 标识。Linux 命令行保留客户机 `ttyS0`，但不预设已知会主动停用 TSC 的 `tsc=unstable`；Linux 继续自行校准和判断 TSC 稳定性。

D3 build 显式设置 `RDRIVE_ACPI_LOAD_AML=0`。该模式仍读取 ACPI 静态表，保留 MCFG、MADT CPU/中断信息和根回调，只跳过 AML 解释器与命名空间设备枚举；它不是捕获任意 AML 错误后继续。当前 smoke 的 `devices.passthrough=[]` 且没有需要 AML 命名空间资源的内核驱动，因此这是满足无物理设备直通 Linux guest 的最小策略。完整 AML 设备枚举支持没有在此 case 中验证。

该 guest 没有物理设备直通，因此使用 `guest_type = "virtualized"`，而不是沿用 ASUS 的 `passthrough` 类型。x86 运行时的 IOAPIC 物理 IRQ 转发只为 `Passthrough` 地址空间启用；纯虚拟 guest 应使用 AxVisor 提供的虚拟设备，不应占用宿主 GSI。当前配置已连续通过两次 2026-11-26 正式 D3 case。

### 2.2 构建与实体板卡运行

`--list` 只发现 case，不申请板卡或执行内核。宿主构建成功也只证明映像、guest 配置和 host initramfs 可以生成；实体板卡验收仍需同一 case 执行到 VM 1 控制台并取得完整 guest 输出。

D3 已配置 AC 来电自启动。两次正式 Linux case 均由 ostool 通过继电器自动停电/上电冷启动，全程无人工操作且两轮之间未改代码；该板卡 case 可按此流程无人值守执行。

```bash
cargo xtask axvisor build \
  --config test-suit/axvisor/board/tianhong-minipc-d3/linux/build-x86_64-unknown-none.toml

cargo xtask axvisor test board \
  --test-case board/tianhong-minipc-d3/linux \
  --board tianhong-minipc-d3-linux \
  --list

cargo xtask axvisor test board \
  --test-case board/tianhong-minipc-d3/linux \
  --board tianhong-minipc-d3-linux \
  --server http://127.0.0.1:2999
```

CI 使用 `target/debug/tg-xtask image pull qemu-x86_64` 和 `target/debug/tg-xtask image pull initramfs-x86_64-busybox.cpio.gz` 将两项资产解压到 `target/axbuild/board-assets/tianhong-minipc-d3-linux`。两条命令均已纳入 D3 CI 检查，实际运行前应从 CI 日志确认资产取得成功。

## 3. 启动自动化与验收

D3 的无人值守启动装配属于 USB 介质编排，不改变 launcher 的槽选择、OTA 确认或 AxVisor 运行时语义。完整装配命令、输入检查和恢复步骤见[组装入口说明](../../../../bootloader/axloader/scripts/d3-uefi-bootstrap/README.md)。

### 3.1 UEFI 自动入口

固定 `edk2-stable202502` 提交 `fbe0805b2091393406952e84724188f8c1941837` 的 Shell 使用 `PcdShellDefaultDelay=0`，并把自身作为 `EFI/BOOT/BOOTX64.EFI`。Shell 优先执行同目录 `startup.nsh`，因此启动卷选择不依赖固定 `FS0`。

[startup.nsh](../../../../bootloader/axloader/scripts/d3-uefi-bootstrap/startup.nsh) 先确认 `EFI/BOOT` 与 `EFI/AXLOADER` 中的入口文件存在，再从 `%homefilesystem%` 选择当前卷并进入 `EFI/D3NET`。脚本依次加载 RNG、Hash2 和 DPC、SNP、MNP、ARP、IP4、UDP4、DHCP4、TCP 八个网络模块，以 `no-report` 运行 [load-network.nsh](../../../../bootloader/axloader/scripts/d3-uefi-bootstrap/load-network.nsh)，最后执行同一卷的 `EFI/AXLOADER/LAUNCHER.EFI`。

自动路径不写诊断日志，也不要求 USB 可写。`assemble-autoboot.sh` 只生成启动编排和驱动目录，不创建或重置 A/B、`STATE`、OTA 清单或其他持久状态；安装到已有 USB 时必须保留这些文件的原始字节。

### 3.2 已验证与待验证

2026-11-26 的正式原 D3 case 连续两轮通过。运行未使用临时诊断入口或额外环境开关，配置为 `Info`、host 16 CPU、300 秒超时、guest 1 CPU、256 MiB 和 `virtualized`；自动 AC 上电、继电器冷启动、USB 外置网络/serial、SerialIo 身份绑定、HTTP 交接、SVM Linux 启动和 BusyBox `test pass!` 在每一轮链路中完成，并进入 `~#` shell。

| 轮次 | Session | 端到端时间 | 结果 |
| --- | --- | --- | --- |
| 第一轮 | `3cc01f10-8fc7-439f-ae59-a744514eadb0` | 163.05 秒 | PASS，退出码 0，全部 groups 通过 |
| 第二轮 | `9bebb339-3cc8-44e2-9cd5-eaaaed79fa62` | 63.80 秒 | PASS，退出码 0，全部 groups 通过 |

两轮使用相同 kernel SHA-256 `324370754f2e374324ea845c2dc3bb94ce8c81ba6c7ab1c6bfa153beb1837466`。端到端时间包含串口身份绑定等待，不用于推导 Linux 启动性能；TSC 在两轮均正常切换。远端 GitHub Actions 尚未执行，因此不能宣称远端 CI job 已通过。

SVM 边界由 `virtualization/axvm/src/arch/x86_64/policy/svm/vcpu.rs` 的 `setup_msr_bitmap()`、`builtin_vmexit_handler()`、`handle_unsupported_mtrr_msr()` 和 `handle_cpuid()` 维护。SVM CPUID 隐藏 MTRR、EXTAPIC 和 IBS；MTRR MSR 读写注入 `#GP(Some(0))` 且不推进 RIP，不伪造 MTRR。标准 APIC/PAT 保持可见，VMX 路径不变。`x86_vlapic` 尚无 EILVT 实现，因此 AMD IBS 与 MTRR 虚拟化不在本 case 的支持范围内。

最终 axvm 21 个 clippy 组合已全部通过，`cargo fmt` 已完成。该静态证据只补充两轮正式板卡 PASS，不扩大 AMD IBS、MTRR 虚拟化或完整 AML 的支持范围；`test pass!` 和 panic 判定保持原样。

### 3.3 CI 清单校验

`resource_group` 是当前 CI schema 的正式字段，值必须为小写 kebab-case，并且只能用于 `board` runner。D3 使用 `resource_group = "tianhong-minipc-d3"` 记录板卡资源类型，具体 `board_id`、电源继电器和并发分配由板卡服务管理。共享继电器 key 与 ASUS 相同不改变这一边界，不增加 CI 锁或专用调度逻辑。

仓库没有 `cargo xtask ci` 子命令，CI 清单规划和路由校验使用已有 Python 入口。规划器默认输出 JSON；前两条命令验证 workflow 路由和 planner suite 契约，第三条命令按调度事件生成当前主 CI 矩阵。

```bash
uv run --python 3.13 --no-project python3 scripts/test/check_ci_routing.py

uv run --python 3.13 --no-project python3 -m unittest discover \
  -s scripts/test -p 'test_ci_*.py'

uv run --python 3.13 --no-project python3 scripts/test/ci_plan.py \
  --mode main \
  --repository rcore-os/tgoskits \
  --repository-owner rcore-os \
  --event-name schedule
```

需要把规划收敛到已提交的 PR 差异时，向 `ci_plan.py` 传入 `--event-name pull_request`、`--head-repository rcore-os/tgoskits` 和明确的 `--since-ref`；工作区未提交文件不在该三点 diff 中，不能据此宣称 PR 路由已验证。
