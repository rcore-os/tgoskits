# StarryOS 测试套件维护指南

本文档说明 `test-suit/starryos/` 的当前目录约定，以及
`scripts/axbuild/src/starry/test.rs` 和 `scripts/axbuild/src/test/` 如何发现、构建和运行这些用例。

## 发现规则

StarryOS 测试按运行方式分为 QEMU 和 board 两类。二者都采用：

```text
test-suit/starryos/<group>/<case>/<runtime-config>.toml
test-suit/starryos/<group>/<build_wrapper>/<case>/<runtime-config>.toml
```

- `<group>` 为 `test-suit/starryos/` 下的一级目录，例如 `normal`、`stress` 或项目新增的测试分组。
- `<case>` 是测试用例名，可以直接位于 group 下，也可以放在 `<build_wrapper>` 下。
- `<build_wrapper>` 用于把同类构建配置和多个 case 放到一个目录中，通常是 `qemu-smp1`、`qemu-smp4`、`board-orangepi-5-plus` 等；如果目录自身同时包含 `build-*` 和 `qemu-*` / `board-*`，它本身也作为 case 发现。
- QEMU 用例通过 `<case>/qemu-<arch>.toml` 发现。
- Board 用例通过 `<case>/board-<board>.toml` 发现。
- 构建配置位于 case 或 build wrapper 的 `build-<target>.toml`，也支持按 arch 匹配的 `build-<arch>.toml`。
- 批量运行时，没有匹配 runtime config 的 case 会被跳过。
- 显式 `-c/--test-case` 时，case 必须在某个可用 build group 中存在，且必须提供当前 arch 对应的 `qemu-<arch>.toml`。
- `-l/--list` 未指定 `--test-group`、`--arch` 或 `--target` 时，会列出所有一级 group 中发现的用例；真实执行未指定 `--test-group` 时仍默认运行 `normal`。

## 当前目录概览

```text
test-suit/starryos/
  normal/
    qemu-smp1/
      build-aarch64-unknown-none-softfloat.toml
      build-loongarch64-unknown-none-softfloat.toml
      build-riscv64gc-unknown-none-elf.toml
      build-x86_64-unknown-none.toml
      smoke/
        qemu-<arch>.toml
      apk-curl/
        qemu-<arch>.toml
      busybox/
        qemu-<arch>.toml
        sh/
          busybox-tests.sh
      python-hello/
        qemu-<arch>.toml
        python/
          test_hello.py
      bugfix/
        qemu-<arch>.toml
        <subcase>/c/CMakeLists.txt
      syscall/
        qemu-<arch>.toml
        <subcase>/c/CMakeLists.txt
      usb/
        qemu-<arch>.toml
        c/
          CMakeLists.txt
          prebuild.sh
          src/
    qemu-smp4/
      build-<target>.toml
      affinity/
        qemu-x86_64.toml
        <subcase>/c/CMakeLists.txt
      test-shm-deadlock/
        qemu-<arch>.toml
        c/CMakeLists.txt
    board-orangepi-5-plus/
      build-aarch64-unknown-none-softfloat.toml
      npu-yolov8/
        board-orangepi-5-plus.toml
      pcie-enumerate/
        board-orangepi-5-plus.toml
  stress/
    stress-ng-0/
      build-<target>.toml
      stress-ng-0/
        qemu-<arch>.toml
```

## QEMU 用例类型

运行器会根据 case 目录内容选择一个 asset pipeline。一个 case 只能使用一种 pipeline。

| Pipeline | 触发条件 | 行为 |
| --- | --- | --- |
| `plain` | 无 `test_commands`，且无 `c/`、`sh/`、`python/` | 直接启动共享 rootfs，并追加 QEMU `-snapshot` |
| `c` | case 目录下存在 `c/` | 使用 CMake 交叉编译，安装产物到 rootfs overlay |
| `sh` | case 目录下存在 `sh/` | 将 shell 脚本注入 `/usr/bin/` |
| `python` | case 目录下存在 `python/` | 在 staging rootfs 中安装 `python3`，并注入 `.py` 文件 |
| `grouped` | `qemu-<arch>.toml` 中存在 `test_commands` | 构建子目录中的 C subcase，生成 `/usr/bin/starry-run-case-tests` 顺序执行命令 |

Pipeline case 会创建每个 case 独立的 rootfs 副本，并把注入后的 rootfs 缓存在：

```text
target/<target>/qemu-cases/<build_group>/<case>/cache/rootfs/
```

plain case 不复制 rootfs，依赖 QEMU `-snapshot` 保证 guest 写入不落回共享镜像。

## QEMU TOML

每个 `qemu-<arch>.toml` 定义运行配置，而不是构建配置。常用字段如下：

| 字段 | 说明 |
| --- | --- |
| `args` | QEMU 参数，`${workspace}` / `${workspaceFolder}` 会解析为仓库根目录 |
| `uefi` | 是否使用 UEFI |
| `to_bin` | 是否把 ELF 转为裸二进制 |
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
    "-device", "virtio-blk-pci,drive=disk0",
    "-drive", "id=disk0,if=none,format=raw,file=${workspace}/target/rootfs/rootfs-riscv64-alpine.img",
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

## Grouped 用例

当多个 guest 程序可以共用同一次 StarryOS 启动时，使用 grouped case：

```text
<case>/
  qemu-<arch>.toml
  <subcase-a>/c/CMakeLists.txt
  <subcase-b>/c/CMakeLists.txt
```

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

运行器会稳定排序子目录、构建 C subcase，并注入 `/usr/bin/starry-run-case-tests`。
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
<group>/<build_group>/
  build-<target>.toml
  <case>/
    board-<board>.toml
```

`board-<board>.toml` 是板测运行配置。发现 board case 后，xtask 会默认映射到：

```text
os/StarryOS/configs/board/<board>.toml
```

并从该 board build config 读取 target。如果当前 build group 下存在匹配的
`build-<target>.toml` 或 `build-<arch>.toml`，则优先使用 test-suit 中的构建配置。

运行示例：

```bash
cargo xtask starry test board --board orangepi-5-plus
cargo xtask starry test board -c pcie-enumerate --board orangepi-5-plus
```

## 运行命令

```bash
# normal QEMU
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --target riscv64gc-unknown-none-elf

# 指定 group 或 case
cargo xtask starry test qemu --arch x86_64 -g normal -c smoke
cargo xtask starry test qemu --arch x86_64 -c affinity

# 列出发现的用例；不指定 group 时列出全部 group
cargo xtask starry test qemu -l
cargo xtask starry test board -l

# stress QEMU
cargo xtask starry test qemu --stress --arch riscv64
cargo xtask starry test qemu -g stress --arch riscv64

# board
cargo xtask starry test board --board orangepi-5-plus
cargo xtask starry test board -g normal -c npu-yolov8 --board orangepi-5-plus
```

## 维护注意事项

- 只为实际验证通过的架构添加 `qemu-<arch>.toml`。
- `qemu-smp1` / `qemu-smp4` 的并发度由 build config 决定；不要只改 QEMU `-smp` 而忘记构建配置。
- `shell_init_cmd` 和 `test_commands` 不能同时使用。
- 一个 case 只能定义一种 pipeline；不要同时放 `c/`、`sh/`、`python/` 或 `test_commands`。
- `success_regex` 选择稳定且唯一的成功行。
- `fail_regex` 保持精确，避免匹配正常输出如 `failed: 0`。
- 不要在同一个工作区并行运行多个 `cargo xtask starry test qemu`，rootfs 和生成配置可能互相影响。
- 新增测试分组时直接在 `test-suit/starryos/` 下添加一级目录；`normal/` 应保持稳定且适合常规 CI，`stress/` 或其他重负载分组可以放置更慢、更重的用例。
