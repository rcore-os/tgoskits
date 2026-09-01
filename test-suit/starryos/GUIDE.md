# StarryOS 测试套件维护指南

本文档说明 `test-suit/starryos/` 的当前目录约定，以及
`scripts/axbuild/src/starry/test.rs` 和 `scripts/axbuild/src/test/` 如何发现、构建和运行这些用例。

## 发现规则

StarryOS test-suit 不再使用 `normal`、`stress` 等一级测试组。QEMU 和 board
用例都直接从 `test-suit/starryos/` 根目录发现：

```text
test-suit/starryos/<case>/<runtime-config>.toml
test-suit/starryos/<build_wrapper>/<case>/<runtime-config>.toml
```

- QEMU 用例通过 `<case>/qemu-<arch>.toml` 发现。
- Board 用例通过 `<case>/board-<board>.toml` 发现。
- 只在非空宿主环境变量存在时才能运行的 Board 用例，在同一 case 目录的
  `requirements.toml` 中声明 `required_env = ["NAME", ...]`。缺失或空值会保留
  `--list` 可见性并输出明确的 skipped 结果，不构建或占用板卡。
- `<build_wrapper>` 用于共享构建配置，例如 `qemu`、`board-orangepi-5-plus`。
- 构建配置位于 case 或最近的 build wrapper 中，文件名为 `build-<target>.toml`。
- 如果目录自身同时包含 `build-*` 和 `qemu-*` / `board-*`，它本身也可以作为 case 被发现。
- 批量运行时，没有当前 arch/runtime config 的目录会被跳过。
- 显式 `-c/--test-case` 时，case 必须存在，且必须提供当前 arch 对应的 runtime config。
- Starry QEMU 支持 `qemu/<subcase>` 作为 `qemu/system` 聚合 case 的单子测例
  选择器；也可以写成 `qemu/system/<subcase>`。
- `<subcase>` 优先使用 `system/` 下的子目录名；如果某个子测例安装的
  `usr/bin/starry-test-suit` binary / CMake target 名与目录名不同，也可以使用唯一的
  binary / target 名，例如 `qemu/test-uid-gid-re-setters` 会映射到
  `qemu/system/syscall-test-uid-gid-re-setters`。
- `-l/--list` 列出根目录下发现的 Starry case；`qemu` 这类仅含 build config 的 wrapper 不会作为 root case 出现。

旧的 Starry `--test-group` 和 `--stress` 入口已经移除。需要运行迁出的压力、K230、
visual 或 golden 类用例时，使用 `cargo xtask starry app ...` 或对应脚本。

## CI 精确路由

CI 路由不改变上述 xtask 发现规则。`.github/ci/checks/starry.toml` 通过 check 下的
`[[check.suite]]` 注册当前可用的 QEMU 架构或 board runner；planner 再以实际存在的
`qemu-<arch>.toml`、`board-<board>.toml` 和最近的 `build-<target>.toml` 解析 case。
不要从目录名字手工拼接 CI job，也不要为未在 CI 中运行的板卡借用其他 runner。

PR 的有效改动全部位于 `test-suit/**` 时，只运行匹配的已注册 case。多个 case 取
稳定去重并集；共享 build wrapper 变更展开到该 wrapper 下所有已注册 case。
`qemu/system/<subcase>` 源码变更会生成 `qemu/<subcase>` selector。若新增 case 或
board 尚未在 manifest 注册，`Plan CI` 会明确失败，而不是静默跳过或自动启用真机。

## 当前目录概览

```text
test-suit/starryos/
  qemu/
    build-aarch64-unknown-none-softfloat.toml
    build-loongarch64-unknown-none-softfloat.toml
    build-riscv64gc-unknown-none-elf.toml
    build-x86_64-unknown-none.toml
    system/
      CMakeLists.txt
      prebuild.sh
      qemu-aarch64.toml
      qemu-loongarch64.toml
      qemu-riscv64.toml
      qemu-x86_64.toml
      syscall-test-brk/
        CMakeLists.txt
        src/
      bugfix-bug-futex-wait-wake/
        CMakeLists.txt
        src/
      c-regression-test-msync/
        CMakeLists.txt
        src/
      drm-test-drm-perbuf-dumb/
        CMakeLists.txt
        src/
      evdev-test-evdev-event-primary/
        CMakeLists.txt
        src/
      usb-audio-iso/
        CMakeLists.txt
        src/
      usb-storage/
        CMakeLists.txt
        src/
  board-orangepi-5-plus/
    build-aarch64-unknown-none-softfloat.toml
    native-hardware-smoke/
      board-orangepi-5-plus.toml
    native-network-smoke/
      board-orangepi-5-plus.toml
      iperf-smoke.sh
```

`qemu/system` 是统一的 SMP4 聚合 QEMU case。`qemu/` 根目录只放四架构 build
config，不放 `qemu-*.toml`。

### AArch64 CI 启动 smoke

AArch64 CI 在运行 `cargo xtask starry test qemu --arch aarch64` 前，会先执行一次
带 `--smp 4` 的普通 `starry qemu` 启动，并通过
`os/StarryOS/configs/qemu/qemu-aarch64-gicv2-boot.toml` 显式选择 GICv2 和 SMP4。
这个 smoke 等待四个 CPU 完成启动并进入 shell 后输出唯一成功标记，用来
覆盖 test-suit 的 GICv3/SMP4 配置没有覆盖到的普通 GICv2 启动路径；它不是
`test-suit/starryos/` 下的可发现 case。

## qemu/system 聚合

`qemu/system/qemu-*.toml` 共用一次 StarryOS 启动，在 SMP4 配置下运行所有系统类
子测例。子测例目录直接放在 `system/` 下，每个子测例只保留自己的资产目录：

```text
qemu/system/<subcase>/
  CMakeLists.txt
  src/
```

子测例目录不要再放 `qemu-*.toml`。架构过滤不能依赖子目录下的 runtime config，而应在代码或 CMake 中显式处理。

`system/qemu-*.toml` 的 `test_commands` 使用 grouped runner 风格，扫描
`/usr/bin/starry-test-suit/*` 并逐个执行。所有子测例通过后打印：

```text
STARRY_GROUPED_TESTS_PASSED
```

system runner 会为每个 binary 单独创建 PID namespace 和 mount namespace。namespace
中的 PID 1 先把挂载传播设为 private，重新挂载该 namespace 独有的 procfs，再 fork
普通测试进程；这样测试不会受到 PID 1 特殊信号语义影响。测试进程结束或超时后，退出
namespace init 会统一终止并回收该 namespace 中的全部后代，包括调用 `setsid()` 逃离
原进程组的进程。不得把跨 binary 的清理建立在 process group 或 session 上。

日志为每个 binary 保留一条开始标记和一条带耗时的完成结果，失败结果还包含退出码；
suite 结束时只打印一条总数、成功数、失败数和总耗时汇总，不再重复输出一份逐项
timing 列表。例如：

```text
STARRY_SYSTEM_TEST_BEGIN: /usr/bin/starry-test-suit/mytest
STARRY_SYSTEM_TEST_PASSED: /usr/bin/starry-test-suit/mytest elapsed_s=1
STARRY_SYSTEM_TEST_SUMMARY: total=1 passed=1 failed=0 elapsed_s=1
```

开始标记用于在超时时定位卡住的 binary；失败时保留该 binary 的原始输出、
`STARRY_SYSTEM_TEST_FAILED`、退出码和耗时。

子测例 CMake 产物应安装到：

```cmake
install(TARGETS mytest RUNTIME DESTINATION usr/bin/starry-test-suit)
```

串口/TTY 事务回归由 `qemu/system/test-tty-termios-transaction` 覆盖。它在真实
`/dev/ttyS0` 上验证配置错误不会发布新 termios，并让普通输出与
`TCSETSW2`/`TCSETSF2` 并发；同时通过 PTY 验证 `TCSETSF2` 在配置事务完成后清理旧输入。
该子目录不放自己的 `qemu-*.toml`，而是由 `qemu/system/qemu-*.toml` 的统一 SMP4 guest
承载。单独运行四个架构时使用：

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu/system/test-tty-termios-transaction
cargo xtask starry test qemu --arch riscv64 -c qemu/system/test-tty-termios-transaction
cargo xtask starry test qemu --arch aarch64 -c qemu/system/test-tty-termios-transaction
cargo xtask starry test qemu --arch loongarch64 -c qemu/system/test-tty-termios-transaction
```

同一 grouped runner 还必须保留 `test-tty-flush` 和
`tty-bugfix-bug-raw-terminal-polling`。前者保持 `TCIFLUSH`、`TCOFLUSH`、`TCIOFLUSH`
以及串口 output flush 的断言，后者保持 raw mode 下 `poll()` 超时的原有成功/失败条件。
`tty-console-input-burst` 继续作为独立 shell-injection case，不迁入 grouped runner。

如果某个 C 子测例只支持部分架构，优先使用 `system/common/starry_arch_filter.cmake`
生成 skip 二进制。skip 输出要清楚说明目标和原因，并返回 0。

## QEMU 参数约定

`system/qemu-x86_64.toml`、`system/qemu-aarch64.toml`、`system/qemu-riscv64.toml`
需要同时覆盖常规系统回归、DRM、evdev 和 USB：

- NVMe 主启动盘 `disk0` 使用 Alpine rootfs，统一配置
  `max_ioqpairs=64,msix_qsize=65`，不回退到 `virtio-blk`。
- `virtio-net` 提供基础网络。
- `virtio-gpu`、`virtio-keyboard`、`virtio-tablet` 支持 DRM/evdev。
- `qemu-xhci,id=xhci,msi=off,msix=off`、`usb-audio`、`usb-storage` 支持 USB 回归。
- USB storage 第二盘使用 `${workspace}/tmp/axbuild/rootfs/rootfs-<arch>-busybox.img`。

`system/qemu-loongarch64.toml` 不带 xHCI、USB audio 或 USB storage。对应 build config
也不启用 `ax-driver/xhci-pci`。USB 测试程序仍会被构建并安装，但在 loongarch64 guest
内立即打印 skip marker 并返回 0，不能访问 USB 设备。

## QEMU 用例类型

运行器会根据 case 目录内容选择一个 asset pipeline。一个 case 只能使用一种 pipeline。

| Pipeline | 触发条件 | 行为 |
| --- | --- | --- |
| `plain` | 无 `test_commands`，且无 `c/`、`sh/`、`python/` | 直接启动共享 rootfs，由 rootfs patcher 仅对主 rootfs drive 设置 `snapshot=on` |
| `c` | case 目录下存在 `c/` | 使用 CMake 交叉编译，安装产物到 rootfs overlay |
| `sh` | case 目录下存在 `sh/` | 将 shell 脚本注入 `/usr/bin/` |
| `python` | case 目录下存在 `python/` | 在 staging rootfs 中安装 `python3`，并注入 `.py` 文件 |
| `grouped` | `qemu-<arch>.toml` 中存在 `test_commands` | 构建子目录资产，注入 grouped runner 需要的 guest 程序 |

Pipeline case 会创建每个 case 独立的 rootfs 副本，并把注入后的 rootfs 缓存在：

```text
target/<target>/qemu-cases/<build_group>/<case>/cache/rootfs/
```

plain case 不复制 rootfs。所有 test-suit pipeline 都由 rootfs patcher 对实际主 rootfs
drive 应用 `snapshot=on`，保证正常退出、panic 或强制终止时 guest 写入不落回源镜像；
不会使用会同时改变 VVFAT ESP、额外数据盘或 pflash 语义的全局 `-snapshot`。
Pipeline 创建的副本只负责资产注入，不承担 QEMU 运行期写隔离。

需要 staging rootfs 的 pipeline 依赖 `debugfs` 和 `fakeroot`。xtask 会在启动
`debugfs rdump` 前检查 EUID；Linux 上还会检查 UID/GID identity mapping 和有效
`CAP_CHOWN`。只有能完整恢复 guest ownership 时才直接提取，否则预先进入
`fakeroot`，避免产生大量权限警告。如果此时缺少 `fakeroot`，xtask 会在启动
`debugfs` 前明确失败，不会先执行再过滤警告或静默回退。

## QEMU TOML

每个 `qemu-<arch>.toml` 定义运行配置，而不是构建配置。常用字段如下：

| 字段 | 说明 |
| --- | --- |
| `args` | QEMU 参数，`${workspace}` / `${workspaceFolder}` 会解析为仓库根目录 |
| `uefi` | 是否使用 UEFI |
| `to_bin` | 是否把 ELF 转为裸二进制 |
| `rootfs_write_policy` | test-suit 只能省略或设为 `"discard"`；`"persist"` 会被拒绝 |
| `shell_prefix` | 等待 guest shell 的提示符 |
| `shell_init_cmd` | plain/C/sh/python case 的 guest 命令 |
| `test_commands` | grouped case 的 guest 命令列表；不能与 `shell_init_cmd` 同时使用 |
| `success_regex` | 全部匹配才 PASS |
| `fail_regex` | 任一匹配即 FAIL |
| `timeout` | 超时时间，单位秒 |

示例：

```toml
args = [
    "-nographic", "-cpu", "rv64",
    "-device", "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65",
    "-drive", "id=disk0,if=none,format=raw,file=${workspace}/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img",
    "-device", "virtio-net-pci,netdev=net0",
    "-netdev", "user,id=net0",
]
uefi = false
to_bin = true
shell_prefix = "root@starry:"
shell_init_cmd = "pwd && echo 'All tests passed!'"
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b']
timeout = 15
```

## C 用例

普通 C case：

```text
<case>/
  qemu-<arch>.toml
  c/
    CMakeLists.txt
    prebuild.sh        # 可选
    src/
      main.c
```

`CMakeLists.txt` 至少应安装可执行文件：

```cmake
cmake_minimum_required(VERSION 3.20)
project(mytest C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(mytest src/main.c)
target_compile_options(mytest PRIVATE -Wall -Wextra -Werror)

install(TARGETS mytest RUNTIME DESTINATION usr/bin)
```

如果该 C case 是 `qemu/system` 的子测例，安装目录必须改成
`usr/bin/starry-test-suit`。

如果需要在 staging rootfs 中安装额外包，可添加 `c/prebuild.sh`：

```sh
#!/bin/sh
set -eu

apk add zlib-dev
```

`prebuild.sh` 通过 qemu-user 在 staging rootfs 中执行。可用环境变量包括：

- `STARRY_STAGING_ROOT`
- `STARRY_CASE_DIR`
- `STARRY_CASE_C_DIR`
- `STARRY_CASE_WORK_DIR`
- `STARRY_CASE_BUILD_DIR`
- `STARRY_CASE_OVERLAY_DIR`

xtask 对 CMake configure、build 和 install 的成功输出默认静默，只保留对应阶段耗时。
任一阶段失败时会回放完整命令、stdout、stderr、退出状态和阶段上下文。`prebuild.sh`
以及 QEMU/guest 输出仍然实时显示，不能依赖成功路径的 CMake 输出作为测试判定标记。

## Grouped 用例

当多个 guest 程序可以共用同一次 StarryOS 启动时，使用 grouped case：

```text
<case>/
  qemu-<arch>.toml
  <subcase-a>/c/CMakeLists.txt
  <subcase-b>/c/CMakeLists.txt
```

`qemu/system` 是特殊的大型 system grouped case：`system/CMakeLists.txt` 是唯一
configure 入口，自动 `add_subdirectory()` 各个 subcase；每个 subcase 直接在目录根放
`CMakeLists.txt` 和 `src/`。如果所有 system subcase 需要共享 rootfs 准备步骤，把脚本
放在 `system/prebuild.sh`，不要给单个 subcase 增加 `prebuild.sh`。

调试单个 system subcase 时，不需要新增 CLI 参数，直接复用 `-c/--test-case`：

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu/syscall-test-uid-gid-re-setters
cargo xtask starry test qemu --arch x86_64 -c qemu/test-futex-race
```

这会继续使用 `qemu/system/qemu-<arch>.toml`，但只配置、编译和注入指定 subcase
目录。

在 `qemu-<arch>.toml` 中使用 `test_commands`：

```toml
shell_prefix = "root@starry:"
test_commands = [
    "/usr/bin/test-a",
    "/usr/bin/test-b",
]
success_regex = ["(?m)^STARRY_GROUPED_TESTS_PASSED\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b', '(?m)^STARRY_GROUPED_TEST_FAILED:']
```

运行器会稳定排序子目录、构建 C subcase，并注入 grouped runner 支持文件。每个命令执行前后都会打印带 `step=当前/总数`、`epoch=`、`status=` 和 `command=` 的标记，例如：

```text
STARRY_GROUPED_TEST_BEGIN: step=1/2 epoch=... command=/usr/bin/test-a
STARRY_GROUPED_TEST_PASSED: step=1/2 epoch=... status=0 command=/usr/bin/test-a
```

如果 grouped case 超时，CI 日志中最后一个 `STARRY_GROUPED_TEST_BEGIN` 通常就是卡住的子命令。
`qemu/system` 使用独立的 `starry-run-system-tests`，日志标记为
`STARRY_SYSTEM_TEST_BEGIN/PASSED/FAILED`，但仍只在全部 binary 通过后打印既有的
`STARRY_GROUPED_TESTS_PASSED`，失败时仍打印 `STARRY_GROUPED_TEST_FAILED`。
共享 runner 默认限制每个 binary 最多运行 120 秒；同步写入密集型的
`test-ext4-inode-unique` 和完成 1400 个磁盘文件清理的 `test-pagecache-cap` 通过显式名称表
取得 240 秒预算。慢用例例外必须保留在共享 runner 中并由静态契约测试覆盖，不能放宽所有
binary 的默认预算。TOML `timeout` 约束整个 QEMU case，不替代上述单 binary 超时。
单 binary 超时后的 PID namespace 清理另有 30 秒硬上限；清理失败必须打印
`STARRY_SYSTEM_TEST_CLEANUP_TIMEOUT` 并立即中止 suite，不能继续运行下一个 binary，也不能
阻塞到外层 QEMU timeout。隔离回归会让持锁后代停在 raw pipe wait，确保 namespace SIGKILL
路径确实强制唤醒并回收这类任务。
目前 grouped Rust subcase 还不支持。

## Shell 和 Python 用例

Shell case 使用 `sh/`：

```text
<case>/
  qemu-<arch>.toml
  sh/
    my-test.sh
```

Python case 使用 `python/`：

```text
<case>/
  qemu-<arch>.toml
  python/
    test_hello.py
```

Python pipeline 会自动在 staging rootfs 中安装 `python3`，再把 `.py` 文件复制到 `/usr/bin/`。

## Board 用例

Board 用例目录结构：

```text
<build_wrapper>/
  build-<target>.toml
  <case>/
    board-<board>.toml
```

`board-<board>.toml` 是板测运行配置。发现 board case 后，xtask 会默认映射到：

```text
os/StarryOS/configs/board/<board>.toml
```

并从该 board build config 读取 target。如果当前 build wrapper 下存在匹配的
`build-<target>.toml`，则优先使用 test-suit 中的构建配置。

板测需要在运行时下载 case 资产时，可在 typed TOML 配置中声明相对于
`board-<board>.toml` 所在目录的文件：

```toml
session_files = [
  "iperf-bench.sh",
  "tools/network/probe.sh",
]
```

路径会在分配板卡前完成规范化和边界检查；绝对路径、`..`、符号链接逃逸、重复路径
和缺失文件都会被拒绝。上传后路径保持不变，不支持 alias 或上传时改名。

`shell_init_cmd` 可使用下列只在活动 board session 内展开的变量：

- `${boardServerIp}`：板端可访问的 ostool-server 地址。
- `${boardServerHttpBaseUrl}`：板端可访问的 session HTTP 基础 URL。
- `${sessionFile:<relative-path>}`：对应共享文件的完整下载 URL。

AKA 的安全 Wi-Fi board case 在构建时从 `STARRY_WIFI_SSID` 和
`STARRY_WIFI_PASSWORD` 生成 AIC station 启动事务。凭据不使用额外 sidecar 或 guest
helper；连接和 DHCP 完成后，脚本仍按上面的普通 HTTP session file 机制下载。runner
只为可信 boot entropy 创建带 `/chosen/rng-seed` 的临时 DTB 副本，不修改仓库 DTB。
空 `STARRY_WIFI_SSID` 只禁用 station 启动连接；AIC8800 驱动仍初始化并注册 `wlan0`，
且其他 AKA board case 继续运行。只有 `wifi-iperf-smoke` 因无法联网而记为 skipped。

普通 shell 变量（例如 `${HOME}`）保持原样。未解析的 session 保留变量会在上板运行
前报错；无论上传、展开还是运行失败，xtask 都会释放 session。

板测需要交叉编译并共享 C 程序时，在 case 下增加 `c/CMakeLists.txt`：

```text
<case>/
  board-<board>.toml
  c/
    CMakeLists.txt
    prebuild.sh        # 可选
    src/
```

xtask 复用 QEMU C case 的 musl staging 和 CMake 工具链，但不会把产物注入 rootfs。
每次运行会创建并清空独立目录：

```text
target/<target>/board-cases/<case>/runs/<run-id>/upload/
```

CMake `install()` 到该 upload root 的所有普通文件都会按原相对路径自动上传，因此构建
产物不需要再写入 `session_files`。例如 `install(... DESTINATION bin)` 对应
`${sessionFile:bin/<program>}`。板端下载、赋权和执行仍必须显式写在
`shell_init_cmd` 中；ostool 不会自动执行上传的程序。upload root 为空、包含符号链接，
或者手写 `session_files` 与 CMake install 产物同路径时会在分配板卡前报错。

位于 `apps/starry/<app>/` 的重型板测仍应保留在 app 目录，不要为了复用共享文件能力
迁入 test-suit。app 下存在 `rust/Cargo.toml` 时，`starry app board` 会复用同一套
musl 交叉编译流水线，把静态程序安装到每次运行独立的
`target/<target>/board-cases/app/<app>/runs/<run-id>/upload/usr/bin/`，并作为 session
文件上传。`init.sh` 通过 `${sessionFile:usr/bin/<program>}` 显式下载、赋权和执行；
该流程不通过 SSH，也不把程序预装到持久 rootfs。

若 Starry 尚无对应板卡网卡驱动，可给 `starry app board` 增加
`--linux-stage`。xtask 会完成同一 app 资产构建，在 `board connect` 的默认 Linux
会话中上传资产并打印 board-visible HTTP URL，而不启动 Starry 内核。操作者应在该
Linux 会话中下载并验证资产，写入 Starry 可见的持久 rootfs 路径并执行 `sync`；
退出会话释放 lease 后，再运行不带 `--linux-stage` 的正常 Starry 板测。这个流程只
改变二进制交付位置，Linux 与 Starry 必须执行同一资产；不得以另一个 shell workload
复用成功标记。

App 的 `board-<name>.toml` 默认复用
`os/StarryOS/configs/board/<name>.toml` 作为内核 build config；只有 app 目录存在
精确的 `build-<target>.toml` 时才覆盖。多个同架构板卡需要不同 SoC feature 时应
省略共享覆盖，避免把某块板的 CPU、MMU 或控制器 feature 注入另一块板。

运行示例：

```bash
cargo xtask starry test board --board orangepi-5-plus
cargo xtask starry test board -c native-hardware-smoke --board orangepi-5-plus
cargo xtask starry app board -t iperf3 -b OrangePi-5-Plus
```

`native-hardware-smoke` 在一次启动中依次验证启动、PCIe、USB2、PWM 和 NPU。
`native-network-smoke` 只执行一条短 TCP TX 命令，随后在 `eth1` 上验证 rtnetlink
地址增删，适合作为 CI 连通性检查。完整吞吐测试位于 `apps/starry/iperf3`，直接通过
上面的 `cargo xtask starry app board` 命令启动板测；ostool server 持续提供 iperf3
服务，board 配置的 `shell_init_cmd` 通过活动 session 的 `${boardServerIp}` 和
`${sessionFile:iperf-bench.sh}` 获取实际地址；app 的 `init.sh` 会按现有 xtask 流程合并
到该命令中，下载并启动测试脚本，不依赖固定网卡、固定 IP、固定网段或额外的板测
启动脚本。

完整 benchmark 固定执行 T01--T07：单流 TX、单流 RX、单流双向、2/4/8 流 TX 和
4 流 RX。每个场景使用 `-t 10 -O 2 -l 128K` 运行 3 次，每个连接结束后固定冷却
15 秒，避免上一轮 TCP teardown 干扰下一轮；脚本直接打印原始输出、中位数和最终
汇总表：

```text
T01  Single-stream DUT TX
Command: iperf3 -c <session-host> -t 10 -O 2 -P 1 -l 128K

Run 1/3
<native iperf3 output>
Result  DUT TX: ... Mbps

Run 2/3
<native iperf3 output>
Result  DUT TX: ... Mbps

Run 3/3
<native iperf3 output>
Result  DUT TX: ... Mbps

Median DUT TX: ... Mbps
STARRY_IPERF3_BENCH_PASSED
```

每轮 iperf3 原始文本和机器可读汇总保存在板端 `/tmp/starry-iperf3-bench/`。
benchmark 只要求所有场景完成并产生有效速率，不设置与机器绑定的吞吐门槛；端口和
测试档位固定，避免不同运行使用不同参数。

ROCK 4D 使用板卡服务名称 `Rock-4D`、仓库内的 RK3576 DTB 和 1,500,000 baud
串口。维护的单核启动回归命令为：

```bash
cargo xtask starry test board -c boot --board rock-4d -b Rock-4D
```

已验证的 8 核启动命令为：

```bash
cargo xtask starry board \
  -c os/StarryOS/configs/board/rock-4d.toml \
  --smp 8 \
  --board-config os/StarryOS/configs/board/rock-4d-board.toml \
  -b Rock-4D
```

两条路径都必须进入 `root@starry:/root #` 并打印独立的
`STARRY_ROCK4D_BOOT_OK` 成功行。RK3576 的固件、PSCI、CPU 拓扑和 CRU/PMU
检查点见 `.agents/skills/arch-platform-porting/references/boot-debugging.md`。

`board-aka-00-sg2002/usb2-libuvc-init` 提供静态交叉编译固定版本上游 libuvc 的
C 资产和 `board-aka-00-sg2002.toml.disabled` 配置模板。该 USB 用例尚未完成
AKA 实板验收，因此模板不会被 board discovery 或 CI 启用。完成验证后可移除
`.disabled` 后缀；其
`shell_init_cmd` 会使用 `wget` 下载程序，并只验证 `uvc_init` / `uvc_exit`，不枚举
摄像头、不采集帧，也不验证 DWC2 isochronous 传输。

## 运行命令

```bash
# QEMU
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --target riscv64gc-unknown-none-elf
cargo xtask starry test qemu --arch x86_64 -c qemu/system
cargo xtask starry test qemu --arch x86_64 -c qemu/syscall-test-uid-gid-re-setters

# 列出发现的用例
cargo xtask starry test qemu -l
cargo xtask starry test board -l

# board
cargo xtask starry test board --board orangepi-5-plus
cargo xtask starry test board -c native-hardware-smoke --board orangepi-5-plus

# 迁出的 heavy app
cargo xtask starry app qemu -t stress/git --arch riscv64
cargo xtask starry app qemu -t k230-qemu/qemu-k230/kpu-smoke --arch riscv64
```

## 维护注意事项

- 只为实际验证通过的架构添加 `qemu-<arch>.toml`。
- `qemu` 的并发度由 build config 决定；不要只改 QEMU `-smp` 而忘记构建配置。
- `shell_init_cmd` 和 `test_commands` 不能同时使用。
- 一个 case 只能定义一种 pipeline；不要同时放 `c/`、`sh/`、`python/` 或 `test_commands`。
- `success_regex` 选择稳定且唯一的成功行。
- `fail_regex` 保持精确，避免匹配正常输出如 `failed: 0`。
- 不要在同一个工作区并行运行多个 `cargo xtask starry test qemu`，rootfs 和生成配置可能互相影响。
- heavy app 不应放回 `test-suit/starryos`；迁出到 `apps/starry` 后加入 `apps/.ignore`，需要时用显式 `-t` 运行。
