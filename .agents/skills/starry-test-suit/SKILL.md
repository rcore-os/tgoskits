---
name: starry-test-suit
description: 在本仓库中新增、重新分组、适配或验证 StarryOS 测试套件用例。处理 `test-suit/starryos`、Starry 的 `qemu-*.toml` 或 `board-*.toml`、QEMU 构建包装目录、分组系统用例、成功或失败匹配规则、C、命令行、Python 或分组用例文件，以及 Starry 测试套件的持续集成或 `xtask` 行为时使用。
---

# Starry 测试套件

## 概述

StarryOS 测试由数据驱动。用例位于 `test-suit/starryos`，发现与执行逻辑主要位于：

- `scripts/axbuild/src/starry/test.rs`
- `scripts/axbuild/src/test/qemu.rs`
- `scripts/axbuild/src/test/case.rs`
- `scripts/axbuild/src/test/build.rs`

QEMU 用例构建 `starryos` 软件包，并运行对应体系结构的 `qemu-<arch>.toml`。板卡用例针对板卡目标构建 StarryOS，再通过板卡运行器执行 `board-<board>.toml`。

## 与其他测试层的边界

- 宿主 `std` 只验证算法、数据结构、状态机、协议解析和错误转换；不能用 fake runtime、fake IRQ、fake timer 或 shell prompt 代替 StarryOS 运行证据。
- ArceOS 的真实调度、IPI、IRQ、timer、SMP、affinity 和上下文切换优先放在 `test-suit/arceos/rust`，使用 `cargo xtask arceos test qemu ...`。
- Starry kernel 私有 Linux ABI、namespace、procfs、pipe、epoll 和内核生命周期语义保留在本 suite 或 Starry kernel axtest；Axvisor 和板卡专有行为分别使用 `cargo xtask ktest qemu`/`cargo xtask ktest board`。
- 同一 crate 可以同时有 std 模型测试和 QEMU/axtest 集成测试，但同一断言只能由一个最接近真实语义的层负责；上层运行证据不能被低层 host 编译替代。

用例的目标风险、必要性、缺陷敏感度与跨层去重先按
[`test-quality`](../test-quality/SKILL.md) 判断；本技能只补充 Starry 测试套件的目录、发现、文件流水线和运行器契约。

宿主 `std` 允许列表和 profile 规则见 [`update-std-tests`](../update-std-tests/SKILL.md)，
ArceOS Rust QEMU 的发现与 runner 契约见 [`arceos-test-adapter`](../arceos-test-adapter/SKILL.md)。

## 工作流程

1. 检查 `test-suit/starryos` 下的目标目录，以及 `scripts/axbuild/src/starry/test.rs` 中当前测试流程。
2. 判断用例运行于 QEMU 还是板卡，再选择根级用例或 `test-suit/starryos` 下的构建包装目录。
3. QEMU 用例可以直接位于 `test-suit/starryos/<case>` 并带匹配的 `build-*.toml`，也可以位于含相应构建配置的包装目录下。只为实际通过的体系结构添加 `qemu-<arch>.toml`。`qemu` 包装目录根部只保存构建配置，`system/` 是分组 QEMU 用例。
4. 客户机需要附加文件时，只能选择一种处理流水线：`c/`、`sh/`、`python/`，或使用子用例目录与 `test_commands` 的分组方式。
5. 板卡用例在用例目录中添加 `board-<board>.toml`，并确认本目录或最近的构建包装目录提供所需 `build-*.toml`。
6. 使用匹配的 `cargo xtask starry test ...` 命令验证。
7. 发现规则、持续集成预期或目录约定变化时，同步更新 `test-suit/starryos/GUIDE.md` 和相关文档。

## 目录规则

- Starry 测试套件不再使用 `normal`、`stress` 等一级测试分组。QEMU 和板卡用例直接从 `test-suit/starryos` 发现。
- QEMU 配置位于 `test-suit/starryos/<case>/qemu-<arch>.toml`，或 `test-suit/starryos/<build-wrapper>/<case>/qemu-<arch>.toml`。
- 板卡配置位于 `test-suit/starryos/<case>/board-<board>.toml`，或 `test-suit/starryos/<build-wrapper>/<case>/board-<board>.toml`。
- 板卡用例依赖非空宿主环境变量时，在同一 case 目录的 `requirements.toml` 中以 `required_env = ["NAME", ...]` 声明。缺失或空值必须明确记为 skipped，不能从发现或 `--list` 中静默删除，也不能构建或占用板卡。
- 构建配置位于用例目录或最近的构建包装目录，命名为 `build-<target>.toml`；存在时也识别 `build-<arch>.toml`。
- 构建包装目录保存共享构建配置和多个用例。目录同时含 `build-*` 与 `qemu-*` 或 `board-*` 时，该目录本身也是用例。
- QEMU 发现先选择具有匹配体系结构或目标构建配置的目录，再在该目录及其下级目录中发现 `qemu-<arch>.toml`。
- 板卡发现扫描 `board-*.toml`，并从用例目录或最近的构建包装目录解析构建配置。
- 批量 QEMU 运行跳过缺少所需 `qemu-<arch>.toml` 的目录。显式 `-c/--test-case` 要求匹配构建包装目录下同时存在用例与配置。Starry QEMU 还接受 `-c qemu/<subcase>` 和 `-c qemu/system/<subcase>`，用于运行 `qemu/system` 中的单个分组子用例。
- 旧的 `--test-group` 和 `--stress` 入口已删除。大型应用、压力、K230、图形和基准图像工作负载位于 `apps/starry`，通过 `cargo xtask starry app ...` 或各自脚本运行。
- `-l/--list` 列出所有发现的 Starry QEMU 或板卡用例。`qemu` 等构建包装目录本身没有运行配置时不进入列表。
- `qemu/system` 是统一 QEMU 构建包装目录下的聚合用例。其子目录只保存文件，不得再放置 `qemu-<arch>.toml`。

## QEMU 文件处理流水线

每个 QEMU 用例最多选择一种流水线：

- `plain`：没有附加文件目录，也没有 `test_commands`。直接启动共享根文件系统；根文件系统修补器只给选中的根磁盘应用 `snapshot=on`。
- `c`：用例目录含 `c/CMakeLists.txt`，通过 CMake 构建并把产物安装到根文件系统叠加层。
- `sh`：用例目录含 `sh/`，把脚本复制到客户机叠加层。
- `python`：用例目录含 `python/`，运行器在准备目录安装 `python3`，并把 `.py` 文件复制到 `/usr/bin/`。
- `grouped`：`qemu-<arch>.toml` 定义 `test_commands`；构建 `<subcase>/c/` 等子目录，并注入 `/usr/bin/starry-run-case-tests` 运行器。`qemu/system` 使用 `system/CMakeLists.txt` 作为一个根 CMake 项目，子用例的 `CMakeLists.txt` 和 `src/` 直接位于 `system/<subcase>`，扫描 `/usr/bin/starry-test-suit/*`，并以 `STARRY_GROUPED_TESTS_PASSED` 为成功标记。`starry-run-system-tests` 为每个程序创建新的进程标识命名空间和挂载命名空间，从命名空间中的进程 1 重新挂载局部 procfs，派生普通测试进程，并依靠命名空间初始化进程退出清理所有后代，包括通过 `setsid()` 脱离的进程。单个分组子用例通过 `-c qemu/<subcase>` 或 `-c qemu/system/<subcase>` 运行。

需要注入文件的用例使用逐用例根文件系统副本，并在 `target/<target>/qemu-cases/.../cache/rootfs/` 缓存。`plain` 用例不复制根文件系统。复制与缓存流水线只负责注入文件；QEMU 写入隔离始终由根文件系统修补器负责。测试配置可省略 `rootfs_write_policy` 或设为 `"discard"`，禁止 `"persist"`。不要使用全局 `-snapshot`，因为它还会改变 VVFAT 固件系统分区、附加磁盘和闪存语义。

## 用例内容

每个 `qemu-<arch>.toml` 只描述运行行为，不描述构建配置：

- `args`：体系结构特定的 QEMU 参数；
- `to_bin` / `uefi`；
- `shell_prefix`；
- `shell_init_cmd`：用于普通、C、命令行或 Python 用例；
- `test_commands`：用于分组用例，不能与 `shell_init_cmd` 同时使用；
- `success_regex`；
- `fail_regex`；
- `timeout`。

较长命令优先使用多行 TOML 字符串。失败匹配规则要收窄，成功标记应稳定且唯一。

## 失败传播

- 本节主要约束 Starry QEMU 测试。板卡测试继续使用 `board-<board>.toml` 和板卡运行器，并通过 `success_regex`、`fail_regex` 及可选 `shell_init_cmd` 判断结果；板卡用例可以复用 C 文件构建器填充会话上传根目录。
- 真实失败必须传播到运行器。不得只打印失败消息，却让 `cargo xtask starry test qemu ...` 成功退出。
- `success_regex` 与 `fail_regex` 必须可靠区分成功和失败。`STARRY_GROUPED_TEST_FAILED` 等失败标记必须被 `fail_regex` 命中；只有所有必需子用例通过后才能输出总成功标记。
- 分组或系统包装脚本中，任一子用例失败都必须输出该子用例失败标记、抑制总成功标记、输出总失败标记，并向外层运行器返回非零结果。
- `qemu/system` 中不同程序不能共享进程标识命名空间或 procfs 挂载。清理必须终止整个命名空间，不能只终止原始进程组或会话，否则后台化或 `setsid()` 后代会把锁和状态泄漏到下一用例。
- 命令行包装脚本应在测试命令后立即保存 `$?`，再赋值、打印日志或清理。`status=failed` 等赋值会把 `$?` 重置为零，过早赋值会隐藏真实退出状态。
- 日志保持紧凑且可追踪：每个程序前输出开始标记，结束时输出包含程序路径和耗时的一条通过或失败结果，最后只输出一次套件汇总。不要在末尾重复逐程序耗时。
- 共享 `qemu/system` 运行器的逐程序默认超时保持 120 秒。只有确实同步密集的程序可以通过运行器内按名称匹配的明确条目延长，并由 axbuild 源码契约测试覆盖；不得提高默认值或把例外复制到各体系结构 TOML。`test-ext4-inode-unique` 和 `test-pagecache-cap` 为 240 秒。
- 逐程序超时后的进程标识命名空间清理另设上限，当前为 30 秒。无法回收命名空间初始化进程时，输出 `STARRY_SYSTEM_TEST_CLEANUP_TIMEOUT`，并在启动下一程序前终止套件。隔离回归中的逃逸后代应阻塞在原始管道等待上，以迫使内核在发布不可捕获终止信号后唤醒它。
- CMake 配置、构建和安装命令成功时保持安静；失败时必须重放命令、标准输出、标准错误、退出状态和阶段上下文。预构建及客户机或 QEMU 输出保持实时。
- 启动 `debugfs` 前先决定根文件系统解压权限。直接执行 `rdump` 需要完整宿主所有权权限，否则先进入 `fakeroot`。Linux 上检查有效用户标识、完整用户与组标识映射以及有效 `CAP_CHOWN`。需要 `fakeroot` 但不可用时，在启动 `debugfs` 前失败；不得先输出再过滤所有权警告，也不得用更弱语义静默重试。
- 只有测试输出清楚跳过标记，且审查或用例注释解释环境为何不能要求成功时，才允许显式跳过。错误修复和回归 QEMU 测试在行为缺失时必须明确失败。

## 编辑规则

- 使用最接近的现有用例作为模板。
- 平台要求未实际变化时，保留体系结构特定启动参数。
- 只在完成对应体系结构验证后添加其配置。
- 同一用例目录不得定义多种流水线。
- C 用例通过 CMake `install()` 安装产物，使其进入客户机叠加层。
- 只有必须在准备根文件系统内执行的软件包或设置才使用 `prebuild.sh`。
- 分组用例的 `test_commands` 要与已安装客户机路径一致，并包含分组成功与失败匹配规则。
- `qemu/system` 的 C 子用例把程序安装到 `usr/bin/starry-test-suit`。共享准备放在 `system/prebuild.sh`，不要放在子用例的 `prebuild.sh`。体系结构特定子用例应生成明确跳过程序或在程序内跳过，不依赖子用例自己的 `qemu-<arch>.toml` 过滤。
- 板卡用例名和配置名应匹配真实板卡，例如 `board-orangepi-5-plus.toml`。
- 板卡用例可声明相对 `board-<board>.toml` 所在目录的 `session_files`。从本地查找到会话端点始终保持原相对路径，不添加别名或远端名称。需要下载会话文件或访问板卡侧服务地址时，在 `shell_init_cmd` 中使用 `${sessionFile:<relative-path>}`、`${boardServerIp}` 或 `${boardServerHttpBaseUrl}`。
- 目标具备可用网络驱动时，临时板卡测试文件默认使用会话文件。网络连接或动态主机配置协议路由可能晚于命令行提示符，应使用有界下载重试。只有目标无法取得会话文件，或正在验证持久共享根文件系统状态时才写入 Linux 根文件系统。
- 含 `c/CMakeLists.txt` 的板卡用例安装到 `target/<target>/board-cases/<case>/runs/<run-id>/upload/`。所有普通文件保持相对路径自动上传。生成文件不要列入 `session_files`；`ostool` 不会自动执行，仍需在 `shell_init_cmd` 中明确写出 `wget`、`chmod` 和运行命令。
- 大型板卡工作负载保留在 `apps/starry`。含 `rust/Cargo.toml` 的 Starry 应用可用 `starry app board` 把静态辅助程序交叉编译到逐次运行的会话上传根目录；其 `init.sh` 必须通过超文本传输协议显式下载 `${sessionFile:usr/bin/<program>}`、设置可执行位并运行，禁止通过安全外壳协议部署或预装到持久根文件系统。

## 验证

使用下列 `xtask` 命令：

```bash
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --arch aarch64 -c qemu/system
cargo xtask starry test qemu --arch x86_64 -c qemu/syscall-test-uid-gid-re-setters
cargo xtask starry app qemu -t stress/git --arch riscv64
cargo xtask starry test board --board orangepi-5-plus
```

修改 `scripts/axbuild` 下 Rust 逻辑时，还要按仓库规则运行格式化和定向静态检查：

```bash
cargo fmt
cargo xtask clippy --package axbuild
```

## 常见错误

- 同一工作区检出中不得并行运行多个 `cargo xtask starry test qemu`。
- `test-suit/starryos` 不是 Cargo 软件包，不要在其中添加 `Cargo.toml` 或 `src/`。
- 不要依靠构建分组名区分 QEMU 与板卡；前者由 `qemu-<arch>.toml` 发现，后者由 `board-<board>.toml` 发现。
- `shell_init_cmd` 与 `test_commands` 互斥。
- 大型应用、压力、K230、图形和基准图像工作负载留在 `apps/starry`，不要移入 `test-suit/starryos`。
- 用例需要对称多处理时，应选用合适的构建分组或配置，例如 `qemu`，不能只添加 QEMU `-smp` 参数。
