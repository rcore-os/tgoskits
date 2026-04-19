# 构建系统说明

本文档系统说明 TGOSKits 当前的 `cargo xtask` 构建入口，覆盖以下内容：

1. 总体架构与调用链路
2. 根命令总表（test / clippy / board / arceos / starry / axvisor）
3. Snapshot 快照机制
4. `--arch` / `--target` 映射关系
5. Build Info 生成与加载
6. 常用命令速查
7. 命令入口选择建议

---

## 1. 总体架构

### 1.1 调用链路

整个构建系统从用户在终端输入 `cargo xtask`（或其别名）开始，经过多层委托和解析，最终由 `ostool` 库完成实际的编译、镜像生成或模拟器启动。下图展示了从 CLI 输入到最终执行的完整调用路径，以及各层之间的职责划分。

```text
cargo xtask / cargo arceos / cargo starry / cargo axvisor / cargo board
    │
    ▼  (.cargo/config.toml aliases)
cargo run -p tg-xtask -- <args>
    │
    ▼  (xtask/src/main.rs → tokio::main)
axbuild::run()
    │
    ▼  (scripts/axbuild/src/lib.rs → Cli::parse)
┌─────────────────────────────────────────────────┐
│ Commands::Test      → test_std::run_std_test     │
│ Commands::Clippy    → clippy::run_workspace_...   │
│ Commands::Board     → board::execute             │
│ Commands::ArceOS    → ArceOS::new()?.execute()   │
│ Commands::Starry    → Starry::new()?.execute()   │
│ Commands::Axvisor   → Axvisor::new()?.execute()  │
└─────────────────────────────────────────────────┘
    │
    ▼  (各子系统内部)
command_flow::resolve_request()  →  解析 arch/target/snapshot → 生成 ResolvedXxxRequest
command_flow::run_build/qemu/uboot()  →  加载 Cargo config  →  调用 ostool Tool API
```

### 1.2 根目录别名（`.cargo/config.toml`）

为了提升日常使用效率，仓库在 `.cargo/config.toml` 中定义了一组命令别名，使得用户可以用更简短的命令触发完整的构建流程。下表列出了所有可用别名及其对应的实际 `cargo` 命令展开形式。

| 别名命令 | 实际展开 |
| --- | --- |
| `cargo xtask ...` | `cargo run -p tg-xtask -- ...` |
| `cargo arceos ...` | `cargo run -p tg-xtask -- arceos ...` |
| `cargo starry ...` | `cargo run -p tg-xtask -- starry ...` |
| `cargo axvisor ...` | `cargo run -p tg-xtask -- axvisor ...` |
| `cargo board ...` | `cargo run -p tg-xtask -- board ...` |

等价关系：`cargo xtask arceos qemu` ≡ `cargo arceos qemu`，以此类推。

### 1.3 源码模块映射

构建系统的实现分布在 `xtask/` 和 `scripts/axbuild/` 两个目录中，按职责划分为多个独立模块。下表梳理了每个源码模块的文件路径和核心职责，便于在需要定位或修改特定功能时快速找到对应代码。

| 层级 | 路径 | 职责 |
| --- | --- | --- |
| 入口 | `xtask/src/main.rs` | Tokio async main，委托 `axbuild::run()` |
| CLI 定义 + 分发 | `scripts/axbuild/src/lib.rs` | `Cli` / `Commands` enum，match 分发 |
| ArceOS 子系统 | `scripts/axbuild/src/arceos/mod.rs` | build / qemu / test / uboot |
| StarryOS 子系统 | `scripts/axbuild/src/starry/mod.rs` | build / qemu / test / rootfs / uboot |
| Axvisor 子系统 | `scripts/axbuild/src/axvisor/mod.rs` | build / qemu / board / test / image / defconfig / config |
| 公共流程 | `scripts/axbuild/src/command_flow.rs` | snapshot 持久化策略、build/qemu/uboot 统一执行 |
| 上下文与类型 | `scripts/axbuild/src/context/` | arch/target 映射、snapshot 文件、请求类型定义 |
| 下载工具 | `scripts/axbuild/src/download.rs` | HTTP 客户端、带进度条的文件下载 |
| Std 测试 | `scripts/axbuild/src/test_std.rs` | CSV 白名单读取、`cargo test -p` 执行 |
| Clippy | `scripts/axbuild/src/clippy.rs` | workspace 包+feature 组合枚举与检查 |
| QEMU 测试框架 | `scripts/axbuild/src/test_qemu.rs` | 测试包列表、目标解析、shell 自动化配置 |
| 板卡管理（公共） | `scripts/axbuild/src/board.rs` | ls / connect / config（通过 ostool-server） |

---

## 2. 根命令总表

`cargo xtask` 下所有命令都是平级的顶层入口，通过 `.cargo/config.toml` 别名均可直接以 `cargo <cmd>` 形式调用。本章按命令类别分组，逐一说明每个命令的功能、参数和执行流程。

### 2.1 `cargo xtask test` — Host Std 测试

该命令用于在宿主机上运行 workspace 中白名单所列包的标准库测试，是验证核心组件（如 `ax-feat`、`axhal` 等）是否正确编译和通过单元测试的统一入口。它不涉及 QEMU 模拟器或交叉编译，完全在开发机原生环境中执行。

**功能**：对 workspace 中白名单内的包逐个执行 `cargo test -p <package>`。

**执行流程**：

```
test_std::run_std_test_command()
  ├── MetadataCommand::new().no_deps().exec()          ← 加载 cargo metadata
  ├── 读取 scripts/test/std_crates.csv                  ← 解析白名单（header: "package"）
  │     └── 校验每个 package 名是否在 workspace_members 中
  └── 对每个 package 执行：
        cargo test -p <package>                         ← 在 workspace_root 下执行
```

**白名单文件格式** (`scripts/test/std_crates.csv`)：
```csv
package
ax-feat
axhal
starry-process
...
```

**要点**：
- 纯 host 测试，不涉及 QEMU 或交叉编译
- 重复 package 名或未知 package 名会报错退出
- 全部通过输出 `all std tests passed`，否则列出失败包并返回错误

---

### 2.2 `cargo xtask clippy` — Workspace 静态检查

该命令对 workspace 中每个包执行 Rust lint 静态分析，覆盖所有命名 feature 的独立组合以及 `docs.rs` 中声明的目标平台。它是保障代码质量的基础设施，任何 clippy 警告都会被视为错误（`-D warnings`），确保合并到主分支的代码符合项目的 lint 规范。

**功能**：枚举 workspace 中每个包的 base + 每个 named feature，对所有 `(package, feature, target)` 组合执行 `cargo clippy -D warnings`。

**执行流程**：

```
clippy::run_workspace_clippy_command()
  ├── MetadataCommand::new().no_deps().exec()
  ├── workspace_packages()                               ← 过滤 workspace_members
  ├── expand_clippy_checks()                             ← 展开 ClippyCheck 列表
  │     对每个 package:
  │       ├── 读取 docs.rs metadata → targets（可选）
  │       ├── 生成 Base check:  clippy -p <pkg> [-D warnings]
  │       └── 对每个非 default feature:
  │             生成 Feature check:  clippy -p <pkg> --no-default-features --features <feat> [-D warnings]
  └── 逐条执行:
        cargo <clippy_args>                              ← 失败立即 bail!("clippy failed for ...")
```

**ClippyCheck 展开规则**：
- 如果 `Cargo.toml` 的 `[metadata.docs.rs]` 中定义了 `targets`，则对每个 target 额外加 `--target <target>`
- 否则使用 `target = None`（单次检查）
- feature 名为 `"default"` 的条目被跳过

---

### 2.3 `cargo xtask board` — 远程开发板管理（公共入口）

该命令提供了与远程 `ostool-server` 交互的能力，用于发现实验室中可用的物理开发板、分配指定类型的板卡并建立串口连接。它是一个独立于各操作系统的公共工具入口，适用于需要在实际硬件上验证的场景。

**功能**：通过 `ostool-server` 进行远程物理板卡的发现、分配和串口连接。

**子命令**：

| 子命令 | 参数 | 说明 |
| --- | --- | --- |
| `ls` | `--server`, `--port` | 列出远端服务器上可用的板卡类型 |
| `connect` | `-b <type>`, `--server`, `--port` | 分配指定类型的板卡并连接串口终端 |
| `config` | （无） | 编辑全局板卡服务器配置 |

**执行流程**（以 `ls` 为例）：
```
board::execute(Command::Ls(server))
  ├── board::load_board_global_config_with_notice()     ← 加载全局 ostool board 配置
  ├── global_config.resolve_server(server, port)         ← 解析 server:port（支持默认值）
  └── board::fetch_board_types(&server, port)            ← HTTP 调用 ostool-server API
        └── board::render_board_table(&boards)           ← 格式化输出表格
```

**注意**：这是顶层公共的 `board` 命令，与 Axvisor 的 `axvisor board` 子命令不同。后者是构建+部署到远端板卡。

---

### 2.4 ArceOS 命令一览

ArceOS 子系统提供了从构建、运行到测试的完整命令集，覆盖了 QEMU 模拟器和 U-Boot 真机两种执行环境。下表列出了所有可用的子命令及其用途说明。

```
cargo xtask arceos <subcommand>
```

| 子命令 | 说明 |
| --- | --- |
| `build [...args]` | 构建 ArceOS 应用 |
| `qemu [...args]` | 构建并在 QEMU 中运行 |
| `test qemu [...args]` | 运行 ArceOS QEMU 测试套件（Rust + C） |
| `test uboot` | 预留入口，当前未实现（直接报 unsupported） |
| `uboot [...args]` | 构建并通过 U-Boot 运行 |

### 2.5 ArceOS 通用参数

ArceOS 的各子命令共享一组通用参数，用于控制构建目标、运行时配置和平台行为。这些参数的值可以来自命令行显式输入、上次命令保存的快照，或系统默认值。下表详细说明了每个参数的含义及其默认值来源。

| 参数 | 短选项 | 说明 | 默认值来源 |
| --- | --- | --- | --- |
| `--package <pkg>` | `-p` | 指定应用包名 | snapshot (`.arceos.toml`) |
| `--arch <arch>` | | 架构别名 | snapshot → `DEFAULT_ARCEOS_ARCH` (= `aarch64`) |
| `--target <tgt>` | `-t` | 完整 target triple | snapshot → 由 arch 推导 |
| `--config <path>` | `-c` | 显式 build info 路径 | 自动推导 |
| `--plat-dyn` | | 启用动态平台链接 | snapshot / target 默认值 |
| `--qemu-config <path>` | | （仅 qemu）覆盖 QEMU 配置 | snapshot |
| `--uboot-config <path>` | | （仅 uboot）覆盖 U-Boot 配置 | snapshot |

### 2.6 `arceos build` — 执行流程

`build` 命令是 ArceOS 构建流程的核心入口。它首先解析命令行参数和快照信息，确定目标架构、包名和构建配置，然后根据需要自动准备运行时资源（如文件系统镜像），最终调用 `ostool` 执行实际的 Cargo 编译。下图展示了完整的执行流程。

```
ArceOS::build(args)
  ├── prepare_request(cli, None, None, Store)            ← 解析请求 + 存储 snapshot
  │     ├── ArceosCommandSnapshot::load(".arceos.toml")   ← 加载上次命令快照
  │     ├── resolve_arceos_arch_and_target(arch, target)  ← arch↔target 双向推导
  │     ├── resolve_build_info_path(package, target, config) ← 生成 build-info 路径
  │     └── store snapshot → .arceos.toml                ← 持久化本次参数
  ├── ensure_package_runtime_assets(&package)             ← 准备运行时资源
  └── run_build_request(request)
        └── command_flow::run_build()
              ├── load_cargo_config(&request)              ← 读取 build-info → Cargo 结构
              └── app.build(cargo, build_info_path)
                    └── tool.cargo_build(&cargo)           ← 调用 ostool 执行构建
```

**ensure_package_runtime_assets** — 运行时资源准备：
- 仅 `arceos-fs-shell` 包需要：自动生成 `test-suit/arceos/rust/fs/shell/disk.img`（64M FAT32）
- 使用 `truncate -s 64M` + `mkfs.fat -F 32` 创建
- 若文件已存在则跳过

**resolve_build_info_path** — Build Info 路径推导：
1. 若 `--config` 显式指定 → 直接使用
2. 否则查找 `<package_dir>/targetinfo/<target>.toml`
3. 再 fallback 到 `<package_dir>/targetinfo/default.toml`

**Build Info 内容**（`ArceosBuildInfo`）：
```toml
[env]           # 环境变量（如 AX_IP, AX_GW）
features = []   # Cargo features
log = "Info"    # 日志级别
max_cpu_num = 4 # 最大 CPU 数（影响 SMP feature）
plat_dyn = true # 动态平台链接
```

### 2.7 `arceos qemu` — 执行流程

`qemu` 命令在 `build` 的基础上增加了 QEMU 模拟器启动环节。它与构建流程共享相同的参数解析和快照逻辑，额外接收一个 QEMU 配置文件路径用于自定义模拟器行为（如 CPU 类型、内存大小、设备参数等）。下图展示了从参数解析到模拟器启动的完整链路。

与 `build` 流程基本一致，额外携带 `qemu_config`：
```
ArceOS::qemu(args)
  ├── prepare_request(cli, args.qemu_config, None, Store)
  ├── ensure_package_runtime_assets(&package)
  └── run_qemu_request(request)
        └── command_flow::run_qemu()
              ├── load_cargo_config(&request)
              ├── load_qemu(request)                       ← QemuRunConfig
              └── app.qemu(cargo, build_info_path, qemu_run_config)
                    └── tool.cargo_run(&cargo, QemuRunnerKind::Qemu{...})
```

**QEMU 配置查找优先级**：
1. 命令行 `--qemu-config <path>`
2. snapshot 中保存的 `qemu.qemu_config`
3. （无默认值，不指定则使用 ostool 内置默认）

### 2.8 `arceos test qemu` — 测试执行流程

`test qemu` 命令是 ArceOS 的自动化回归测试入口。它不是简单地运行单个应用，而是按照预定义的测试包列表，在 QEMU 中逐一执行每个测试用例（包括 Rust 测试和 C 测试），通过正则匹配输出结果来判定 pass/fail，最终汇总报告。用户可以通过 `--only-rust` 或 `--only-c` 选择只运行其中一类测试。

```
ArceOS::test(ArgsTest { command: TestCommand::Qemu(args) })
  ├── planned_qemu_test_flows(&args)                      ← 根据 --only-rust/--only-c 决定
  │     默认: [Rust, C]                                   ← 两者都跑
  │     --only-rust: [Rust]
  │     --only-c: [C]
  │
  ├── [Rust flow] → test_rust_qemu(args):
  │     ├── parse_arceos_test_target(&args.target)        ← 解析 arch + target
  │     └── 对 ARCEOS_TEST_PACKAGES 中的 15 个包逐一执行:
  │           ├── ensure_package_runtime_assets(package)
  │           ├── resolve_test_qemu_config(package, target)
  │           │     └── 查找 <package_dir>/qemu-{arch}.toml
  │           ├── prepare_request(..., SnapshotPersistence::Discard)  ← 不存 snapshot
  │           └── run_qemu_request(request)
  │                 └── 匹配 success/fail 正则判断结果
  │
  └── [C flow] → test_c_qemu(args):
        ├── discover_c_tests("test-suit/arceos/c/")       ← 发现 C 测试目录
        │     预定义列表:
        │       helloworld, memtest, httpclient,
        │       pthread/{basic,parallel,pipe,sleep}
        ├── prepare_c_test_cargo_config(...)               ← 生成临时 .cargo/config.toml
        └── 对每个 C 测试:
              ├── 编译（通过生成的 cargo config）
              └── 在 QEMU 中运行 + 结果匹配
```

**Rust 测试包列表**（`ARCEOS_TEST_PACKAGES`，共 15 个）：
```
arceos-memtest, arceos-exception, arceos-affinity,
arceos-net-echoserver, arceos-net-httpclient, arceos-net-httpserver,
arceos-irq, arceos-parallel, arceos-priority,
arceos-fs-shell, arceos-sleep, arceos-tls,
arceos-net-udpserver, arceos-wait-queue, arceos-yield
```

**C 测试列表**（`C_TEST_NAMES`，共 7 个）：
```
helloworld, memtest, httpclient,
pthread/basic, pthread/parallel, pthread/pipe, pthread/sleep
```

**支持的测试目标**：
| Arch | Target Triple |
| --- | --- |
| `x86_64` | `x86_64-unknown-none` |
| `aarch64` | `aarch64-unknown-none-softfloat` |
| `riscv64` | `riscv64gc-unknown-none-elf` |
| `loongarch64` | `loongarch64-unknown-none-softfloat` |

**测试结果判定**：每个包的 QEMU 输出通过正则匹配判断 pass/fail，最终汇总报告。

### 2.9 `arceos uboot` — 执行流程

`uboot` 命令用于将 ArceOS 应用通过 U-Boot 引导加载器部署到真实硬件上运行。它在构建流程的基础上，将执行后端从 QEMU 模拟器切换为 `ostool` 的 U-Boot runner，适用于需要在物理开发板上验证的场景。

与 `qemu` 类似，但使用 `command_flow::run_uboot()` 调用 ostool 的 U-Boot runner：
```
ArceOS::uboot(args)
  ├── prepare_request(cli, None, args.uboot_config, Store)
  ├── ensure_package_runtime_assets(&package)
  └── run_uboot_request(request)
        └── tool.cargo_run(&cargo, CargoRunnerKind::Uboot { uboot_config })
```

---

### 2.10 StarryOS 命令一览

StarryOS 子系统在 ArceOS 的基础上增加了 rootfs 镜像管理能力，因为 StarryOS 作为完整操作系统需要文件系统镜像才能正常运行。下表列出了所有可用的子命令，其中 `rootfs` 是 StarryOS 特有的独立命令。

```
cargo xtask starry <subcommand>
```

| 子命令 | 说明 |
| --- | --- |
| `build [...args]` | 构建 StarryOS |
| `qemu [...args]` | 构建并在 QEMU 中运行 |
| `rootfs [--arch <arch>]` | **下载并准备 rootfs 镜像到 target 目录** |
| `test qemu [...args]` | 运行 StarryOS QEMU 测试 |
| `test uboot` | 预留入口，未实现 |
| `uboot [...args]` | 构建并通过 U-Boot 运行 |

### 2.11 StarryOS 通用参数

StarryOS 的参数体系与 ArceOS 基本一致，但不支持 `--package`（因为 StarryOS 始终构建固定的 `starryos` 包）。下表列出了所有可用参数及其默认值来源。

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `--arch <arch>` | 架构别名 | `DEFAULT_STARRY_ARCH` (= `riscv64`) |
| `--target <tgt>` | 完整 target triple | 由 arch 推导 |
| `--config <path>` | build info 路径 | 自动推导 |
| `--qemu-config <path>` | （仅 qemu）QEMU 配置覆盖 | 无 |
| `--uboot-config <path>` | （仅 uboot）U-Boot 配置覆盖 | 无 |

### 2.12 `starry build` / `qemu` / `uboot` — 执行流程

StarryOS 的构建和运行流程与 ArceOS 结构相似，但有三个关键差异：始终构建固定的 `starryos` 包、使用独立的 `.starry.toml` 快照文件、以及在 QEMU 运行时自动确保 rootfs 镜像可用。下图展示了 `qemu` 命令的完整执行路径，其中 rootfs 准备是 StarryOS 特有的步骤。

与 ArceOS 结构类似，核心差异：
- **固定包名**：StarryOS 始终构建 `starryos` 包（`STARRY_PACKAGE`）
- **Snapshot 文件**：`.starry.toml`（而非 `.arceos.toml`）
- **qemu 额外步骤**：自动确保 rootfs 镜像存在（见下文 4.5 节）

```
Starry::qemu(args)
  ├── prepare_request((&args.build).into(), ...)
  ├── default_qemu_args(workspace_root, &request)          ← ★ 自动准备 rootfs
  │     └── ensure_rootfs_in_target_dir(...)               ← 下载/解压 rootfs（见 4.5）
  │     └── qemu_args_for_disk_image(disk_img)             ← 生成 virtio-blk/net QEMU 参数
  └── run_qemu_request_with_args(request, qemu_args)
```

**default QEMU 参数**（附加到 QEMU 命令行）：
```
-device virtio-blk-pci,drive=disk0
-drive id=disk0,if=none,format=raw,file=<rootfs.img>
-device virtio-net-pci,netdev=net0
-netdev user,id=net0
```

### 2.13 `starry rootfs` — Rootfs 下载流程

`rootfs` 是 StarryOS 特有的独立命令，负责从远程服务器下载预构建的文件系统镜像到本地 `target/` 目录。该命令也可以被 `qemu` 和 `test qemu` 自动触发——当检测到本地缺少对应架构的 rootfs 时，会在运行前自动执行下载。下图展示了完整的下载和解压流程。

**这是独立命令，也可被 `qemu` / `test qemu` 自动触发。**

```
Starry::rootfs(args)
  ├── arch = args.arch.unwrap_or(DEFAULT_STARRY_ARCH)      ← 默认 aarch64
  ├── target = starry_target_for_arch_checked(&arch)       ← arch → target 映射
  └── ensure_rootfs_in_target_dir(workspace_root, &arch, &target)
```

**ensure_rootfs_in_target_dir 详细流程**：

```
ensure_rootfs_in_target_dir(workspace_root, arch, target)
  ├── target_dir = workspace_root/target/<target>/          ← 如 target/aarch64-unknown-none-softfloat/
  ├── rootfs_name = "rootfs-{arch}.img"                   ← 如 rootfs-aarch64.img
  ├── rootfs_xz = "{target_dir}/{rootfs_name}.xz"
  │
  ├── if rootfs_img 已存在 → 直接返回路径
  │
  └── else (需要下载):
        url = "https://github.com/Starry-OS/rootfs/releases/download/20260214/{rootfs_name}.xz"
        ├── download_with_progress(url, &rootfs_xz)        ← HTTP 下载（带进度条）
        │     └── download_to_path_with_progress(client, url, output_path)
        │           ├── reqwest::Client (connect_timeout=30s, total_timeout=30min)
        │           └── indicatif ProgressBar 显示下载进度
        └── decompress_xz_file(&rootfs_xz, &rootfs_img)    ← xz2 解压
              └── XzDecoder → 逐块写入 img 文件
```

**下载源信息**：
| 项目 | 值 |
| --- | --- |
| Base URL | `https://github.com/Starry-OS/rootfs/releases/download/20260214` |
| 文件格式 | `{rootfs-{arch}.img}.xz`（xz 压缩的 raw disk image） |
| 存放位置 | `{workspace}/target/{target}/rootfs-{arch}.img` |
| 支持架构 | x86_64, aarch64, riscv64, loongarch64 |

### 2.14 `starry test qemu` — 测试执行流程

StarryOS 的 QEMU 测试直接构建 `starryos`，并从 `test-suit/starryos/normal/<case>/qemu-<arch>.toml` 或 `test-suit/starryos/stress/<case>/qemu-<arch>.toml` 发现测例。批量模式会扫描当前组下所有一级子目录，只执行存在 `qemu-<arch>.toml` 的 case；显式 `-c/--test-case` 则要求该目录和当前架构配置都存在，否则直接报错。`${workspace}` / `${workspaceFolder}`、`shell_init_cmd`、`success_regex`、`fail_regex`、`timeout` 仍全部由 case 自己的 QEMU 配置文件决定。

```
Starry::test_qemu(args)
  ├── parse_test_target(&args.target)                          ← 解析 arch + target
  ├── choose test group                                        ← 默认 normal, --stress 切到 stress
  ├── discover_qemu_cases(arch, args.test_case, group)         ← 在当前组发现/筛选 case
  ├── write_default_qemu_defconfig_for_target(target)
  ├── prepare_request(test_build_args(arch), ...)             ← package 固定是 starryos
  ├── ensure_rootfs_in_target_dir(...)                         ← 确保共享 rootfs 存在
  ├── load_cargo_config(request)                               ← 准备基础 Cargo 配置
  ├── for case in cases
  │     ├── copy shared rootfs to per-case rootfs
  │     ├── optional case `c/`
  │     │     ├── extract staging rootfs
  │     │     ├── optional `c/prebuild.sh`
  │     │     ├── cmake --build
  │     │     ├── cmake --install → overlay
  │     │     └── inject overlay back into per-case rootfs
  │     └── app.qemu(cargo, request.build_info_path, case.qemu_config_path)
  │            └── ostool 直接读取 case qemu 配置并运行
  └── finalize_qemu_case_run(...)                            ← 总是打印成功/失败/耗时汇总
```

**test qemu 特有参数**：

| 参数 | 说明 |
| --- | --- |
| `-t, --target <arch>` | 目标架构（必填） |
| `-c, --test-case <case>` | 只运行指定测例；不传则运行该架构下全部匹配测例，缺少当前架构配置的目录在批量模式下会被跳过 |
| `--stress` | 切换到 `stress` 组；默认运行 `normal` 组 |

**关键设计**：测试把运行判据完全下沉到分组测例目录，并直接透传 case `qemu-<arch>.toml` 给 `ostool`；如果 case 提供 `c/`，则由 `axbuild` 统一负责 `prebuild.sh`、CMake 构建、install overlay 与 rootfs 回写，而不是在 `axbuild` 内对具体 case 名做构建特判。

### 2.15 `starry test board` — 远程板测执行流程

StarryOS 的远程板测同样从 `test-suit/starryos/normal/<case>/` 发现测例，但只扫描 `board-<name>.toml`。每个 `board-<name>.toml` 只保存板测运行配置，构建配置并不复制到 test-suit，而是固定映射到 `os/StarryOS/configs/board/<name>.toml`。当前首个预置 group 是 `smoke-orangepi-5-plus`。

```
Starry::test_board(args)
  ├── ensure_board_test_args(args)                          ← 显式 config 必须配合 --test-group
  ├── discover_board_test_groups(args.test_group)           ← 扫描 normal/*/board-*.toml
  │     └── group 名 = <case>-<board>
  │     └── build config = os/StarryOS/configs/board/<board>.toml
  ├── for group in groups
  │     ├── prepare_request(config=group.build_config_path, target=group.target, ...)
  │     ├── load_board_config(group.board_test_config_path)
  │     └── app.board(cargo, build_info_path, board_config, RunBoardOptions { ... })
  └── finalize_board_test_run(...)                          ← 按 group 汇总结果
```

**test board 特有参数**：

| 参数 | 说明 |
| --- | --- |
| `-t, --test-group <group>` | 只运行指定板测组；不传则运行全部已发现组 |
| `--board-test-config <path>` | 覆盖 group 自带的 board run config；要求同时传 `--test-group` |
| `-b, --board-type <type>` | 覆盖 board 类型 |
| `--server <host>` | 覆盖 ostool-server 地址 |
| `--port <num>` | 覆盖 ostool-server 端口 |

**关键设计**：Starry 的板测把运行判据放在 `test-suit/starryos`，但继续复用 `os/StarryOS/configs/board/*.toml` 作为唯一的 build config 来源，这样 `board-*` 和 `qemu-*` 在 test-suit 中只负责“怎么测”，不负责“怎么构建”。

---

### 2.16 Axvisor 命令一览

Axvisor 是三个子系统中命令最丰富的，因为它不仅需要管理 hypervisor 自身的构建和运行，还需要处理 Guest 镜像管理、板级配置生成、远程板卡部署等额外职责。下表列出了所有可用的子命令。

```
cargo xtask axvisor <subcommand>
```

| 子命令 | 说明 |
| --- | --- |
| `build [...args]` | 构建 Axvisor hypervisor |
| `qemu [...args]` | 构建并在 QEMU 中运行 Axvisor |
| `board [...args]` | 构建并部署到远程开发板运行 |
| `test qemu [...args]` | 运行 Axvisor QEMU 测试 |
| `test uboot -b <board> [...]` | 运行 Axvisor U-Boot 板测 |
| `test board -t <group> [...]` | 运行 Axvisor 远程板卡测试组 |
| `uboot [...args]` | 构建并通过 U-Boot 运行 |
| `defconfig <board>` | 生成指定板级的默认配置 |
| `config ls` | 列出所有可用板级配置名 |
| `image ls [--verbose] [pattern]` | 列出可用 Guest 镜像 |
| `image pull <image> [--output-dir dir] [--no-extract]` | 下载并解压 Guest 镜像 |

### 2.17 Axvisor 通用参数

Axvisor 在通用参数基础上增加了 `--vmconfigs`（VM 配置文件列表）和板卡相关参数（`--board-type`、`--server`、`--port`），以支持 hypervisor 管理多个虚拟机的场景。下表列出了所有参数及其默认值。

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `--arch <arch>` | 架构别名 | `DEFAULT_AXVISOR_ARCH` (= `aarch64`) |
| `--target <tgt>` | 完整 target triple | 由 arch 推导 |
| `--config <path>` | board/build config 路径 | 自动推导 |
| `--plat-dyn` | 动态平台链接 | target 默认值 |
| `--vmconfigs <path...>` | VM 配置文件列表（可多个） | 空 |
| `--qemu-config <path>` | （仅 qemu）QEMU 配置模板 | 自动推导 |
| `--uboot-config <path>` | （仅 uboot）U-Boot 配置 | 无 |
| `--board-config <path>` | （仅 board）板级运行配置 | 无 |
| `--board-type / -b <type>` | （仅 board/test）板卡类型 | 无 |
| `--server <host>` | （仅 board/test）ostool-server 地址 | 全局配置 |
| `--port <num>` | （仅 board/test）ostool-server 端口 | 全局配置 |

### 2.18 `axvisor build` — 执行流程

`build` 命令负责编译 Axvisor hypervisor 二进制。与 ArceOS 不同，Axvisor 的构建配置来自板级配置文件（包含环境变量、feature 列表和 VM 配置列表），且需要通过 `AxvisorContext` 定位 axvisor 子包目录。下图展示了完整的构建请求解析和执行过程。

```
Axvisor::build(args)
  ├── prepare_request((&args).into(), None, None, Store)
  │     ├── AxvisorContext::new()                          ← 初始化（定位 axvisor 目录）
  │     ├── AxvisorCommandSnapshot::load(".axvisor.toml")  ← 加载快照
  │     ├── resolve_axvisor_arch_and_target(arch, target)
  │     ├── resolve_build_info_path(axvisor_dir, target, config)
  │     │     优先: --config → os/axvisor/.build.toml → os/axvisor/targetinfo/{target}.toml
  │     └── store snapshot → .axvisor.toml
  │
  └── run_build_request(request)
        └── load_cargo_config(&request)                     ← 读取 AxvisorBoardConfig
              ├── env, features, log, max_cpu_num, plat_dyn
              └── vm_configs (从 board config 合入)
```

**Build Config 来源**（优先级从高到低）：
1. `--config <path>` 显式指定
2. `os/axvisor/.build.toml`（defconfig 生成或手动编辑）
3. `os/axvisor/targetinfo/{target}.toml`（per-target 默认）

**AxvisorBoardConfig** 结构：
```toml
[arceos]          # 复用 ArceosBuildInfo
env = { AX_IP = "..." }
features = ["fs", "rk3568-clk"]
log = "Info"
plat_dyn = true
max_cpu_num = 4

vm_configs = ["configs/vms/linux-aarch64-qemu-smp1.toml"]  # VM 配置列表
```

### 2.18 `axvisor qemu` — 执行流程（含自动 Guest 资源准备）

`qemu` 命令在构建 Axvisor 后启动 QEMU 模拟器运行 hypervisor。它的一个关键特性是**自动 Guest 资源准备**：当无法从 vmconfig 推断出 rootfs 路径时，会按目标架构自动下载对应的 Guest 镜像。下图展示了从请求解析、rootfs 推断/下载到 QEMU 启动的完整流程。

```
Axvisor::qemu(args)
  ├── prepare_request(...)
  │
  ├── infer_rootfs_path(&request.vmconfigs)?                ← 从 vmconfig 推导 rootfs
  │     └── 读取每个 vmconfig toml → [kernel].kernel_path → 同目录 rootfs.img
  │
  ├── if rootfs 未找到:
  │     prepare_default_rootfs_for_arch(&ctx, &arch)        ← ★ 自动下载 Guest 镜像
  │           见 5.9 节
  │
  └── run_qemu_request(request)
        └── app.qemu(cargo, build_info_path, QemuRunConfig {
              qemu_config: os/axvisor/scripts/ostool/qemu-{arch}.toml,
              default_args: { to_bin, args: [...] },        ← 含 rootfs 路径
              override_args: { ... },                        ← vmconfig 覆盖
            })
```

**默认 QEMU 运行参数按架构**：

| 架构 | 关键参数 |
| --- | --- |
| `aarch64` | `-cpu cortex-a72 -machine virt,virtualization=on,gic-version=3 -smp 4 -m 8g` |
| `riscv64` | `-cpu rv64 -machine virt -bios default -smp 4 -m 4g` |
| `x86_64` | `-cpu host -machine q35 -smp 1 -accel kvm -m 128M` |
| `loongarch64` | `-smp 4 -m 4g` |

**rootfs 路径推断逻辑** (`infer_rootfs_path`)：
1. 遍历 `--vmconfigs` 指定的每个 vmconfig 文件
2. 解析 TOML → `[kernel].kernel_path`
3. 取 `kernel_path` 的父目录 + `rootfs.img`
4. 若该文件存在 → 使用该路径

### 2.19 `axvisor board` — 远程板卡部署

`board` 命令将构建好的 Axvisor 部署到远程物理开发板上运行。它通过 `ostool-server` 完成板卡分配、镜像传输和串口管理，适用于在真实硬件上进行端到端验证的场景。

```
Axvisor::board(args)
  ├── prepare_request(...)
  ├── load_cargo_config(&request)
  └── app.board(cargo, build_info_path, RunBoardArgs {
        config, board_config, board_type, server, port
      })
        └── tool.cargo_run_board(&cargo, args)             ← 通过 ostool-server 部署
```

### 2.20 `axvisor defconfig` / `config`

`defconfig` 和 `config` 命令用于管理 Axvisor 的板级配置。`defconfig` 从预定义的板级配置模板生成默认的 `.build.toml` 文件，而 `config ls` 列出所有可用的板级名称。这些命令简化了为不同硬件平台准备构建配置的过程。

**`defconfig <board>`**：
```
config::write_defconfig(workspace_root, axvisor_dir, &board)
  ├── resolve_board(axvisor_dir, board_name)               ← 查找 os/axvisor/configs/board/{name}.toml
  ├── 复制 → os/axvisor/.build.toml
  └── 更新 .axvisor.toml snapshot 中 config 路径
```

**`config ls`**：
```
config::available_board_names(axvisor_dir)
  └── 列出 os/axvisor/configs/board/ 下的所有 .toml 文件名（去掉扩展名）
```

### 2.21 `axvisor test qemu` — 测试执行流程（含自动 Guest 资源下载）

`test qemu` 是 Axvisor 的自动化测试入口，与普通 `qemu` 命令不同，它包含完整的测试编排逻辑：按目标架构自动下载 Guest 镜像、生成测试专用 VM 配置、配置 shell 自动化交互（等待 shell 就绪→发送测试命令→匹配输出判定结果）。下图展示了 aarch64 和 x86_64 两种架构的测试准备差异和统一执行路径。

```
Axvisor::test_qemu(args)
  ├── parse_axvisor_test_target(&args.target)              ← aarch64 / x86_64
  │
  ├── [aarch64]:
  │     prepare_linux_aarch64_guest_assets(&ctx)            ← ★ 下载 Linux Guest 镜像
  │           见 5.9.1 节
  │     → 返回 PreparedLinuxGuestAssets { image_dir, generated_vmconfig, rootfs_path }
  │
  ├── [x86_64]:
  │     prepare_nimbos_x86_64_guest_vmconfig(&ctx)         ← ★ 下载 NIMBOS Guest 镜像
  │           见 5.9.2 节
  │     → 返回 vmconfig 路径
  │
  ├── prepare_request(..., SnapshotPersistence::Discard)   ← 测试不存 snapshot
  ├── default_qemu_config_template_path(...)               ← os/axvisor/scripts/ostool/qemu-{arch}.toml
  ├── axvisor_test_shell_config(arch)                       ← Shell 自动化配置
  │     aarch64: prefix="~ #", init_cmd="pwd && echo 'guest test pass!'"
  │              success=["guest test pass!"], fail=[panic, kernel panic, ...]
  │     x86_64:  prefix=">>", init_cmd="hello_world"
  │              success=["Hello world from user mode program!"]
  └── shell_autoinit_qemu_override_args(request, shell)
        └── 合并 QEMU 模板参数 + shell 自动化参数
        └── app.qemu(...)
```

**Shell 自动化测试机制**：
- QEMU 启动后等待 `shell_prefix` 出现（表示 guest shell 就绪）
- 发送 `shell_init_cmd` 作为测试命令
- 输出匹配 `success_regex` → pass；匹配 `fail_regex` → fail

### 2.22 `axvisor test uboot` / `test board`

除了 QEMU 模拟器测试外，Axvisor 还支持在真实硬件上运行测试。`test uboot` 通过 U-Boot 将 Axvisor 部署到预定义的物理板卡上并验证，而 `test board` 则通过 `ostool-server` 在远程板卡上执行预配置的测试组。两者都使用预定义的板卡配置和 VM 配置组合。

**test uboot**：
```
axvisor_uboot_board_config(&args.board)                    ← 查找预定义板卡配置
  支持的板卡:
  - orangepi-5-plus   (build: configs/board/orangepi-5-plus.toml, vm: linux-aarch64-orangepi5p-smp1)
  - phytiumpi         (build: configs/board/phytiumpi.toml,       vm: linux-aarch64-e2000-smp1)
  - roc-rk3568-pc     (build: configs/board/roc-rk3568-pc.toml,   vm: linux-aarch64-rk3568-smp1)

→ uboot_test_build_args(board.build_config, board.vmconfig)
→ prepare_request(..., uboot_config, SnapshotPersistence::Discard)
→ run_uboot_request(request)
```

**test board**：
```
axvisor_board_test_groups(args.test_group)                 ← 查找预定义测试组
  支持的组:
  - phytiumpi-linux
  - orangepi-5-plus-linux
  - roc-rk3568-pc-linux

  每组包含: build_config, vmconfigs[], board_test_config

→ 对每组中每个 vmconfig 依次执行:
  board_test_build_args(group)
  → app.board(cargo, build_info_path, RunBoardArgs { board_test_config, board_type, server, port })
```

### 2.23 Guest 镜像管理系统（`axvisor image` + 自动下载）

Axvisor 引入了一套完整的 Guest 镜像管理机制，用于解决 hypervisor 运行时所需的 Guest 操作系统内核和 rootfs 的获取问题。该系统由本地存储、远程 registry 和自动同步三部分组成，支持手动 `image pull` 和命令执行时的自动下载两种使用方式。下图展示了镜像系统的整体架构和文件组织。

#### 5.9.1 镜像系统架构

```
.image.toml                          ← Workspace 根目录的镜像配置文件
  ├── local_storage = "<path>"       ← 本地存储目录（默认: $TEMP/.axvisor-images）
  ├── registry = "<url>"            ← Registry URL（默认: GitHub axvisor-guest registry）
  ├── auto_sync = true              ← 自动同步开关
  └── auto_sync_threshold = 604800  ← 同步阈值秒数（默认 7 天）

{local_storage}/
  ├── images.toml                   ← 本地缓存的镜像索引（registry 副本）
  ├── .last_sync                    ← 上次同步时间戳
  └── {image-name}/                 ← 各镜像的下载缓存
        ├── {archive}.tar.gz        ← 下载的压缩包
        └── {extract-dir}/          ← 解压后的内容
              ├── qemu-aarch64      ← Guest kernel
              ├── rootfs.img        ← Guest rootfs
              └── axvm-bios.bin     ← （仅 x86_64 nimbos）
```

#### 5.9.2 `image ls` — 列出可用镜像

`image ls` 命令查询本地镜像存储中所有可用的 Guest 镜像，支持按名称过滤和详细模式输出。执行时会自动检查 registry 是否需要同步（基于配置的时间阈值），并在必要时从远程拉取最新的镜像索引。

```
image::list_images(ctx, overrides, ArgsLs { verbose, pattern })
  ├── ImageConfig::read_config(workspace_root)              ← 读取 .image.toml
  ├── overrides.apply_on(&mut config)                       ← 应用 -S/-R/-N 覆盖
  ├── Storage::new_from_config(&config)                     ← 初始化存储（含 auto-sync）
  │     └── new_with_auto_sync(path, registry, threshold)
  │           ├── 尝试 Storage::new(path) → 加载本地 images.toml
  │           └── 若失败或超过阈值:
  │                 new_from_registry(registry, path)       ← 从网络拉取 registry
  │                       ├── resolve_bootstrap_source()     ← 解析 bootstrap 源
  │                       │     默认: https://github.com/.../axvisor-guest/.../registry/default.toml
  │                       │     回退: .../registry/v0.0.22.toml
  │                       └── ImageRegistry::fetch_with_includes()
  │                             ├── HTTP GET registry URL
  │                             └── 递归处理 [[includes]] 引用的子 registry
  └── storage.image_registry.print(verbose, pattern)
```

**Registry 数据源**（支持 include 链）：
```toml
# default.toml (主 registry)
includes = [{ url = "https://.../registry/images.toml" }]   # 可引用外部 registry

images = [
  { name = "linux", version = "0.0.1", arch = "aarch64",
    url = "https://...", sha256 = "...", ... },
  ...
]
```

#### 5.9.3 `image pull <spec>` — 下载镜像

`image pull` 命令从远程 registry 下载指定版本的 Guest 镜像到本地存储，默认还会自动解压。下载过程包含完整性校验（SHA256）和原子写入（通过 `.part` 临时文件 + rename），确保即使中断也不会留下损坏的文件。

```
image::pull_image(ctx, overrides, ArgsPull { image, output_dir, no_extract })
  ├── ImageSpecRef::parse(&image)                          ← 解析 "name" 或 "name:version"
  ├── Storage::new_from_config(&config)
  ├── storage.resolve_image(spec)                           ← 在本地 registry 中查找
  │     └── 按 name (+ version) 匹配 ImageEntry
  ├── storage.pull_image(spec, output_dir, extract)
  │     ├── ensure_archive(image, &archive_path)            ← 确保压缩包存在
  │     │     ├── if 存在且 sha256 校验通过 → 跳过
  │     │     └── else:
  │     │           download_to_path_with_progress(url, .part)  ← 下载到 .part 临时文件
  │     │           sha256 校验 → 重命名为正式文件名
  │     │
  │     └── if extract:
  │           extract_archive(archive, extract_dir)          ← tar.gz 解压
  │                 ├── GzDecoder → Archive::new()
  │                 └── 逐文件解压到目标目录
  │
  └── 返回解压目录路径（或 archive 路径 if --no-extract）
```

**下载与校验安全措施**：
1. 下载到 `.part` 临时文件，完成后 rename（原子操作）
2. SHA256 校验不匹配 → 删除重下载
3. 校验失败 → 删除 part 文件并报错
4. 网络异常 → 清理 part 文件并传播错误

#### 5.9.4 自动下载场景汇总

除了手动执行 `image pull` 外，多个命令在运行时会根据需要自动触发 Guest 镜像或 rootfs 的下载。下表汇总了所有自动下载场景、触发条件和下载内容的对应关系，帮助理解何时需要网络访问以及会下载什么。

以下命令会在运行时**自动触发** Guest 镜像下载（无需手动 `image pull`）：

| 触发命令 | 条件 | 下载的镜像 | 用途 |
| --- | --- | --- | --- |
| `axvisor qemu` | vmconfig 中无法推断 rootfs | 按 arch 选择 Guest 镜像 | 运行时 rootfs |
| `axvisor test qemu --target aarch64` | 始终 | `qemu_aarch64_linux` (Linux Guest) | 测试用 Guest |
| `axvisor test qemu --target x86_64` | 始终 | `qemu_x86_64_nimbos` (NIMBOS Guest) | 测试用 Guest |
| `starry qemu` | 始终 | StarryOS rootfs (来自 GitHub Releases) | 运行时磁盘 |
| `starry test qemu` | 始终 | StarryOS rootfs (共享 target 镜像) | 测试用磁盘 |
| `starry rootfs` | 始终 | StarryOS rootfs | 单独准备 |

**预定义 Image Spec 与架构映射**：

| Image Spec Name | 架构 | 内容 |
| --- | --- | --- |
| `qemu_aarch64_linux` | aarch64 | Linux Guest kernel + rootfs |
| `qemu_riscv64_arceos` | riscv64 | ArceOS Guest + rootfs |
| `qemu_x86_64_nimbos` | x86_64 | NIMBOS Guest kernel + BIOS + rootfs |

---

## 3. Snapshot 快照机制

### 3.1 设计目的

三套子系统都会将用户最近一次命令的关键参数持久化到仓库根目录的 TOML 文件中，使得后续命令可以省略重复输入的参数。这种快照机制不是"锁定"配置，而是作为一种便捷的默认值补全策略——显式命令行参数始终优先于快照值。

持久化用户最后一次命令的参数选择，使得后续命令可以**省略重复参数**。

### 3.2 Snapshot 文件

每个子系统维护独立的快照文件，使用对应的结构体进行序列化和反序列化。下表列出了三个子系统的快照文件路径和对应的 Rust 结构体。

| 子系统 | 文件路径 | 结构体 |
| --- | --- | --- |
| ArceOS | `{workspace}/.arceos.toml` | `ArceosCommandSnapshot` |
| StarryOS | `{workspace}/.starry.toml` | `StarryCommandSnapshot` |
| Axvisor | `{workspace}/.axvisor.toml` | `AxvisorCommandSnapshot` |

### 3.3 Snapshot 内容

快照文件以 TOML 格式保存，包含命令的核心参数（arch、target、package 等）以及子命令特有的运行时配置路径。以下展示了 ArceOS 快照文件的完整结构和各字段含义。

以 ArceOS 为例 (`.arceos.toml`)：
```toml
package = "ax-helloworld"              # --package
arch = "aarch64"                       # --arch
target = "aarch64-unknown-none-softfloat"  # --target
plat_dyn = true                        # --plat-dyn

[qemu]
qemu_config = "path/to/qemu.toml"      # --qemu-config

[uboot]
uboot_config = "path/to/uboot.toml"    # --uboot-config
```

### 3.4 参数解析优先级（由高到低）

当同一参数在多个来源都有值时，系统按以下优先级选择使用哪一个：命令行显式输入最高，系统默认值最低。这种设计确保了用户始终可以通过显式参数覆盖任何自动推导的值。

```
命令行显式参数 (--arch, --target, --package 等)
  > Snapshot 中保存的值
  > 系统默认值 (DEFAULT_*_ARCH / DEFAULT_*_TARGET)
```

### 3.5 Snapshot 持久化策略

并非所有命令都会更新快照文件。正常的使用命令（build、qemu、uboot）会保存参数以便后续复用，而测试命令（test qemu）则有意跳过持久化，避免测试用的一次性参数污染用户的常规工作配置。下表汇总了不同场景的策略。

| 场景 | 策略 | 说明 |
| --- | --- | --- |
| `build` / `qemu` / `uboot` | `SnapshotPersistence::Store` | 正常命令：解析后写入 snapshot |
| `test qemu` | `SnapshotPersistence::Discard` | 测试命令：不写 snapshot（避免污染用户偏好） |

---

## 4. Arch / Target 映射关系

### 4.1 统一映射表

三套子系统使用统一的架构别名到 Rust target triple 的映射关系。用户可以使用简短的架构别名（如 `aarch64`）或完整的 target triple（如 `aarch64-unknown-none-softfloat`），系统会自动处理双向推导。下表列出了所有支持的架构及其对应的 target。

| Arch Alias | Target Triple | 默认 Arch |
| --- | --- | --- |
| `aarch64` | `aarch64-unknown-none-softfloat` | 所有三个系统的默认 |
| `x86_64` | `x86_64-unknown-none` | - |
| `riscv64` | `riscv64gc-unknown-none-elf` | - |
| `loongarch64` | `loongarch64-unknown-none-softfloat` | - |

### 4.2 解析规则

`--arch` 和 `--target` 参数的设计遵循互斥原则——指定了其中一个后，另一个会自动推导。这种设计避免了用户同时传入冲突值的情况，同时也允许灵活的输入方式。以下是完整的解析规则说明。

- `--arch` 和 `--target` **互斥**：指定了其中一个，另一个会被忽略
- 仅指定 `--arch` → 自动查表得到 target
- 仅指定 `--target` → 反向查表得到 arch
- 都不指定 → 使用 snapshot 值 → 最终使用默认值
- `--target` 也接受完整 target triple 作为 `test qemu` 的参数

### 4.3 各系统默认值

当用户不指定任何架构或目标时，各子系统会使用各自的默认值。当前三个系统的默认架构均为 `aarch64`，对应的 target triple 为 `aarch64-unknown-none-softfloat`。下表汇总了各系统的默认配置。

| 系统 | 默认 Arch | 默认 Target |
| --- | --- | --- |
| ArceOS | `aarch64` | `aarch64-unknown-none-softfloat` |
| StarryOS | `aarch64` | `aarch64-unknown-none-softfloat` |
| Axvisor | `aarch64` | `aarch64-unknown-none-softfloat` |

---

## 5. Build Info 生成与加载

### 5.1 ArceOS Build Info 路径查找顺序

ArceOS 的 build info 文件包含特定包在特定目标平台下的构建参数（环境变量、features、日志级别等）。系统按以下优先级顺序查找该文件：显式 `--config` 参数优先，然后是包目录下的 per-target 配置，最后 fallback 到默认配置。

```
--config <path>                           ← 最高优先级
  > {package_dir}/targetinfo/{target}.toml  ← per-target
  > {package_dir}/targetinfo/default.toml   ← fallback
```

### 5.2 StarryOS Build Info 路径查找顺序

StarryOS 的 build info 文件位于 `os/StarryOS/` 目录下，查找优先级与 ArceOS 相同：显式配置路径优先，然后是 per-target 配置，最后使用默认值。

```
--config <path>
  > os/StarryOS/targetinfo/{target}.toml
  > os/StarryOS/targetinfo/default.toml
```

### 5.3 Axvisor Build Info 路径查找顺序

Axvisor 的 build info 查找略有不同：`defconfig` 命令生成的配置文件位于 `os/axvisor/.build.toml`（不包含 target 后缀），这是 Axvisor 的首选默认位置。如果该文件不存在，则 fallback 到 per-target 配置。

```
--config <path>
  > os/axvisor/.build.toml                  ← defconfig 生成位置
  > os/axvisor/targetinfo/{target}.toml
```

### 5.4 Build Info → Cargo Config 转换

Build info 文件中的 TOML 配置在加载后需要转换为 `ostool` 库使用的 `Cargo` 结构体。这个转换过程不仅涉及简单的字段映射，还包括 feature 名称的自动解析（如根据 `plat_dyn` 值选择 `plat-dyn`/`myplat`/`defplat`）和环境变量的注入。下图展示了转换后的 `Cargo` 结构体各字段含义。

加载 Build Info TOML 后，转换为 ostool 的 `Cargo` 结构体：

```
ArceosBuildInfo → Cargo {
    env: HashMap<String, String>,       # 环境变量
    target: String,                     # Rust target triple
    package: String,                    # Cargo 包名
    features: Vec<String>,              # Cargo features（含 plat-dyn/myplat/defplat/smp）
    log: Option<LogLevel>,              # 日志级别 → AX_LOG 环境变量
    extra_config: Option<PathBuf>,      # 额外 .cargo/config 片段
    args: Vec<String>,                  # 额外 cargo 构建参数
    pre_build_cmds / post_build_cmds,   # 构建前后钩子命令
    to_bin: bool,                       # 是否重命名输出二进制
}
```

**Feature 自动解析规则**（`resolve_features`）：
1. 移除显式的 `plat-dyn` / `defplat` / `myplat`（及带前缀版本）
2. 若 `plat_dyn == true` → 添加 `{prefix}plat-dyn`
3. 否则若原 features 含 `myplat` → 添加 `{prefix}myplat`
4. 否则 → 添加 `{prefix}defplat`
5. 若 `max_cpu_num > 1` → 添加 `{prefix}smp`
6. Prefix 检测：根据包的直接依赖判断使用 `ax-std/` 还是 `ax-feat/` 前缀

---

## 6. 常用命令速查

### 6.1 ArceOS

以下命令展示了 ArceOS 从构建到测试的典型工作流：构建指定包、在 QEMU 中运行、以及执行完整的回归测试套件。

```bash
cargo xtask arceos build --package ax-helloworld --arch riscv64
cargo xtask arceos qemu --package ax-helloworld --arch riscv64
cargo xtask arceos test qemu --target riscv64
```

### 6.2 StarryOS

StarryOS 的典型工作流需要先准备好 rootfs 镜像（首次运行时会自动下载），然后才能进行构建和测试。

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry build --arch riscv64
cargo xtask starry qemu --arch riscv64
cargo xtask starry test qemu --target riscv64
```

### 6.3 Axvisor

Axvisor 的工作流通常从生成板级配置开始，然后构建 hypervisor、在 QEMU 中验证，最后管理 Guest 镜像。

```bash
cargo xtask axvisor defconfig qemu-aarch64
cargo xtask axvisor build --arch aarch64
cargo xtask axvisor qemu --arch aarch64
cargo xtask axvisor test qemu --target aarch64
cargo xtask axvisor image ls
cargo xtask axvisor image pull qemu_aarch64_linux
```

### 6.4 Host / std

Host 级别的标准库测试和静态分析可以直接在开发机上运行，不涉及任何模拟器或交叉编译。

```bash
cargo xtask test
cargo xtask clippy
```

---

## 7. 命令入口选择建议

面对不同的开发和验证需求，选择合适的命令入口可以提高工作效率。下表根据典型场景推荐了最匹配的命令，帮助快速定位到所需的工具链。

根据不同的开发和验证场景，可以选择最合适的命令入口。下表汇总了各场景的推荐命令及其适用范围。

| 场景 | 推荐命令 |
| --- | --- |
| 组件开发后的最小运行验证 | `cargo xtask <os> qemu ...` |
| 系统级回归验证 | `cargo xtask <os> test qemu ...` |
| Host / std crate 回归 | `cargo xtask test` |
| Guest 镜像管理 | `cargo xtask axvisor image ...` |
| 远程开发板测试 | `cargo xtask axvisor board ...` 或 `cargo xtask axvisor test board ...` |

---

## 相关文档

- [quick-start.md](quick-start)
- [components.md](components)
- [arceos-guide.md](/docs/design/systems/arceos-guide)
- [starryos-guide.md](/docs/design/systems/starryos-guide)
- [axvisor-guide.md](/docs/design/systems/axvisor-guide)
- [repo.md](repo.md)
