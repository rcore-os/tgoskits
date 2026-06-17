# StarryOS BusyBox 兼容性测试报告

## 1. 概述

本报告记录 StarryOS BusyBox 兼容性测试用例的当前结构、判定逻辑、本次新增覆盖项以及验证结果。

BusyBox 测试位于：

```text
apps/starry/qemu/busybox
```

该用例通过 StarryOS QEMU 测试框架启动 Alpine rootfs，进入 guest 后执行：

```text
/usr/bin/busybox-tests.sh
```

推荐验证命令为：

```bash
cargo xtask starry test qemu --arch riscv64 -c busybox
```

本次工作在既有 BusyBox applet 覆盖基础上，新增了一批更偏向真实 shell 脚本和 Linux 用户态兼容性的语义测试，重点覆盖文件系统路径、权限、硬链接、文本处理、参数展开和 shell 环境行为。

最终 riscv64 QEMU 验证结果为：

```text
PASS: 284  FAIL: 0  TOTAL: 284
```

修改前基线为：

```text
PASS: 272  FAIL: 0  TOTAL: 272
```

因此本次新增了 12 个稳定 BusyBox 语义测试，且没有引入既有 BusyBox 测试回归。

## 2. 测试目录结构

BusyBox case 当前包含以下文件：

```text
apps/starry/qemu/busybox/
  qemu-aarch64.toml
  qemu-loongarch64.toml
  qemu-riscv64.toml
  qemu-x86_64.toml
  sh/
    busybox-tests.sh
```

其中：

- `qemu-*.toml` 定义不同架构下 QEMU 启动参数、shell 入口、success matcher、fail matcher 和超时时间；
- `sh/busybox-tests.sh` 是注入 guest rootfs 后执行的 BusyBox 测试脚本；
- `qemu-smp1` 是构建 wrapper，复用上层 `build-*.toml`，以单核 QEMU 环境运行该 normal case。

该 case 使用 StarryOS test-suit 的 shell asset pipeline。测试运行前，框架会把 `sh/busybox-tests.sh` 注入 rootfs 的：

```text
/usr/bin/busybox-tests.sh
```

随后由 `shell_init_cmd` 自动执行。

## 3. QEMU 配置与 matcher

四个架构配置均使用同一个 guest 入口：

```toml
shell_init_cmd = "/usr/bin/busybox-tests.sh"
```

成功判定要求同时匹配：

```toml
success_regex = ["(?m)^PASS: \\d+  FAIL: 0", "(?m)^Test run completed"]
```

这意味着测试脚本必须打印总计行，并且 `FAIL` 计数必须为 0。同时还需要打印稳定的结束标记：

```text
Test run completed
```

失败判定包含 panic 捕获：

```toml
'(?i)\bpanic(?:ked)?\b'
```

本次将 aarch64、loongarch64、x86_64 的 BusyBox fail matcher 与 riscv64 对齐，加入单项失败捕获：

```toml
'(?m)^FAIL: '
```

这样即使总计行由于异常退出没有打印，只要某个单项测试输出了：

```text
FAIL: <name>
```

QEMU runner 也能及时判定该 case 失败。

需要注意的是，本次只完整验证了 riscv64 QEMU。其他架构仅同步 matcher 配置，不宣称已经通过完整运行验证。

## 4. busybox-tests.sh 组织方式

`busybox-tests.sh` 使用简单的计数器维护测试结果：

```sh
PASS=0; FAIL=0; SKIP=0
```

每个测试通常遵循以下结构：

1. 使用 `timeout 10` 包裹待测命令，避免 guest 内命令卡死；
2. 将 stdout 和 stderr 都收集到 `_t`；
3. 使用明确条件判断输出、退出状态或字节序列；
4. 成功时打印 `PASS: <case>` 并递增 `PASS`；
5. 失败时打印 `FAIL: <case>`，输出原始 `_t`，并递增 `FAIL`。

脚本末尾打印总计：

```sh
echo "=== BusyBox Test Summary ==="
echo "PASS: $PASS  FAIL: $FAIL  TOTAL: $((PASS+FAIL))"
_m1="Test"; _m2="run"; _m3="completed"; echo "$_m1 $_m2 $_m3"
```

测试风格强调以下原则：

- 每个 applet 或语义点独立 PASS/FAIL；
- 不依赖真实时间、网络、随机性或外部设备；
- 所有临时文件放在 `/tmp`；
- 运行前清理对应临时路径；
- 对可能阻塞的命令加 `timeout`；
- 对字节级输出使用 `od` 后精确比较；
- 不使用过宽的 `grep` 条件造成误判；
- 失败时打印命令原始输出，便于定位 StarryOS syscall、fs 或 shell 行为问题。

## 5. 本次新增测试项

本次新增 12 个 BusyBox 语义测试，全部位于 `busybox-tests.sh` 末尾的附加语义测试段：

```text
Additional stable BusyBox semantics for shell-script compatibility.
```

新增测试并不是重复已有 PR 的 chown、cpio、dos2unix、env、split、tail、tar、xxd 等 applet 覆盖，而是继续补充更常见的 shell 脚本兼容行为。

### 5.1 busybox_touch_no_create

验证命令：

```sh
busybox touch -c /tmp/bb_sem_touch_missing
```

验证语义：

- `touch -c` 对不存在的文件不应创建新文件；
- 随后用 `busybox test ! -e` 确认文件仍不存在。

该行为对 configure 脚本和构建系统较重要，因为它们经常使用 `touch -c` 尝试只更新既有文件时间戳。

### 5.2 busybox_rm_recursive

验证命令：

```sh
busybox rm -rf /tmp/bb_sem_rm
```

验证语义：

- 能递归删除嵌套目录；
- 能删除目录中的普通文件；
- 删除后目标路径不存在；
- 多级路径创建、写入、删除流程都走真实文件系统行为。

该测试覆盖 `mkdir`、`open/write`、`rm -rf`、目录项删除和路径解析的组合行为。

### 5.3 busybox_ln_hardlink

验证命令：

```sh
busybox ln /tmp/bb_sem_ln_a /tmp/bb_sem_ln_b
busybox stat -c %h /tmp/bb_sem_ln_a
busybox cat /tmp/bb_sem_ln_b
```

验证语义：

- `ln` 能创建 hard link；
- 源文件 link count 变为 2；
- 通过 hard link 读取到的内容与源文件一致。

该测试可以暴露 tmpfs、VFS 或底层文件系统中 hardlink inode 共享、link count 更新、目录项缓存不一致等问题。

### 5.4 busybox_readlink_exact

验证命令：

```sh
busybox ln -s /tmp/bb_sem_rl_target /tmp/bb_sem_rl_link
busybox readlink /tmp/bb_sem_rl_link
```

验证语义：

- `readlink` 输出必须精确等于 symlink 中存储的目标路径；
- 不接受只包含目标文件名或额外日志的宽松匹配。

该测试关注 symlink 内容读取，不要求 `readlink -f` 解析最终真实路径。

### 5.5 busybox_realpath_dotdot

验证命令：

```sh
busybox realpath /tmp/bb_sem_real/./d/..//d
```

验证语义：

- `realpath` 能处理 `.`；
- 能处理 `..`；
- 能处理连续 `/`；
- 输出规范化后的绝对路径：

```text
/tmp/bb_sem_real/d
```

该测试对 StarryOS path resolution 尤其重要。大量用户态脚本会生成包含 `.`、`..` 或重复分隔符的路径。

### 5.6 busybox_stat_mode_size

验证命令：

```sh
busybox stat -c "%s %a %F" /tmp/bb_sem_stat
```

验证语义：

- 文件大小为 3；
- 文件权限为 640；
- 文件类型为 `regular file`。

期望输出精确为：

```text
3 640 regular file
```

该测试覆盖 `stat` 返回的 size、mode 和 file type 字段。它比只 grep `File:` 更能发现权限字段或文件类型字段不正确的问题。

### 5.7 busybox_chmod_symbolic

验证命令：

```sh
busybox chmod u=rw,g=r,o= /tmp/bb_sem_chmod
busybox stat -c %a /tmp/bb_sem_chmod
```

验证语义：

- symbolic chmod 能正确解析 `u=rw,g=r,o=`；
- 最终权限为 640；
- `chmod` 与 `stat` 权限字段一致。

该测试覆盖权限修改 syscall、文件元数据更新和 BusyBox symbolic mode parser 的组合行为。

### 5.8 busybox_sort_unique

验证命令：

```sh
busybox printf 'b\nA\nb\n' | busybox sort -u
```

验证语义：

- `sort -u` 能排序；
- 能去重；
- 输出顺序精确为：

```text
A
b
```

测试中将换行规范化为 `|` 后比较：

```text
A|b|
```

这样避免只 grep `A` 或 `b` 导致误判。

### 5.9 busybox_uniq_counts

验证命令：

```sh
busybox printf 'a\na\nb\n' | busybox uniq -c
```

验证语义：

- `uniq -c` 能统计连续重复行；
- 输出经空格规范化后必须为：

```text
2 a|1 b|
```

该测试验证文本处理中的管道、stdin/stdout、计数输出和 sed 空白规范化。

### 5.10 busybox_xargs_n1

验证命令：

```sh
busybox printf 'aa\nbb\n' | busybox xargs -n1 busybox printf '<%s>\n'
```

验证语义：

- `xargs -n1` 每次只传一个参数；
- 子进程多次执行；
- 输出精确为：

```text
<aa>|<bb>|
```

该测试能覆盖 pipe、fork/exec、argv 构造、wait 和 stdout 回收路径。

### 5.11 busybox_printf_escape

验证命令：

```sh
busybox printf '%b' 'a\012b' | busybox od -An -tx1
```

验证语义：

- `%b` 能解析八进制转义；
- `\012` 被转换为 LF；
- 字节序列精确为：

```text
61 0a 62
```

该测试采用 `od` 做字节级检查，避免只 grep 字符串时漏掉换行、CRLF 或转义解析错误。

### 5.12 busybox_sh_env_cd

验证命令：

```sh
busybox sh -c 'export BB_SEM_ENV=ok; cd /tmp && [ "$BB_SEM_ENV:$PWD" = "ok:/tmp" ] && command -v busybox >/dev/null && busybox echo sh_env_cd_ok'
```

验证语义：

- `sh -c` 能执行复合命令；
- `export` 设置环境变量；
- `cd` 更新当前目录；
- `$PWD` 与实际目录一致；
- `[` 条件表达式可用；
- `command -v busybox` 能找到 busybox；
- 成功输出精确标记。

该测试对后续移植 shell 脚本、configure 脚本和包管理脚本很关键。

## 6. 涉及的 StarryOS 行为

这批 BusyBox 测试虽然位于用户态 applet 层，但实际会间接覆盖多类 StarryOS 内核和文件系统行为：

- `open` / `read` / `write`；
- `mkdir` / `unlink` / `rmdir`；
- `link` / `symlink` / `readlink`；
- `stat` / `chmod`；
- path resolution 中的 `/`、`.`、`..` 和重复 `/`；
- pipe 和标准输入输出重定向；
- `fork` / `execve` / `wait`；
- shell 环境变量和当前工作目录；
- tmpfs 或 rootfs overlay 中的临时文件读写。

本次新增测试没有暴露新的 StarryOS bug，因此没有修改内核、VFS 或 syscall 代码。

## 7. 验证过程

所有关键验证均在 Docker 镜像 `b7c4600e825d` 中完成。

### 7.1 基线验证

修改前运行：

```bash
docker run --rm --privileged -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo xtask starry test qemu --arch riscv64 -c busybox'
```

结果：

```text
PASS: 272  FAIL: 0  TOTAL: 272
```

### 7.2 修改后 BusyBox 验证

修改后运行：

```bash
docker run --rm --privileged -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo xtask starry test qemu --arch riscv64 -c busybox'
```

结果：

```text
PASS: 284  FAIL: 0  TOTAL: 284
```

QEMU runner 输出：

```text
ok: busybox
starry normal qemu summary:
passed (1):
  busybox
failed (0):
  <none>
```

### 7.3 语法与格式检查

shell 语法检查：

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'sh -n apps/starry/qemu/busybox/sh/busybox-tests.sh'
```

结果：通过。

格式检查：

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo fmt'
```

结果：通过，无额外 Rust 改动。

diff 空白检查：

```bash
git diff --check
```

结果：通过。

### 7.4 clippy 验证

按照仓库要求，运行了 StarryOS 包 clippy：

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo xtask clippy --package starryos'
```

结果：

```text
clippy summary: 1 package(s), 5 check(s), 1 package(s) passed, 0 package(s) failed
passed checks: 5, failed checks: 0
all clippy checks passed
```

## 8. 当前限制

当前仍有以下限制：

- 本次只完整验证了 riscv64 QEMU；
- aarch64、loongarch64、x86_64 仅同步了 BusyBox fail matcher，没有宣称完整通过；
- BusyBox 脚本仍是单文件组织，后续测试继续增长后可以按 applet 类型拆分；
- 某些既有测试仍偏 smoke test 风格，只检查命令能运行或输出包含某个宽松字符串；
- 本次新增测试刻意控制在 12 个稳定语义点，没有覆盖网络、真实时间、交互终端或随机输出相关 applet。

## 9. 后续建议

后续可以继续沿以下方向扩展 BusyBox 兼容性：

- 将高价值 applet 按文件系统、文本处理、shell、进程管理分组；
- 为已有宽松测试补充更严格的语义断言；
- 增加 applet coverage summary，自动对比 `busybox --list` 与测试项；
- 在 aarch64、loongarch64、x86_64 上分别运行 BusyBox case 后再声明多架构通过；
- 对新增失败项优先定位真实 StarryOS syscall、VFS、tmpfs 或 exec/wait 行为，而不是降低测试有效性。
