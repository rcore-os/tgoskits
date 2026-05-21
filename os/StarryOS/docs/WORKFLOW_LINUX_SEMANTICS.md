# Workflow: Linux 语义功能的 PR 流程

适用范围：任何修改 syscall / 信号 / 进程 / 线程 / VFS / 文件锁 / 内存映射 /
futex 等 Linux/POSIX 语义的工作。本流程在 `CONTRIBUTING.md` 之上追加要求。

写这份文档的动因，是 PR #273（feat(starry): support multi-threaded execve）
的代价——21 轮 review、28 个 commit、近一个月反复打磨，几乎每一个被揭出来的
错误都属于"先读一遍 Linux 内核源码就能避免"的类别。reviewer 替我们做了
fault enumeration 和 alignment 校验，本应是 author 自己提交前的工作。

对照 PR #246（feat(starry): implement per-process credentials subsystem）：
4 commit、2 轮 review、smooth landing。差异不在难度，在准备方式——#246 的
PR body 第一节就是「问题描述」+「根本原因分析」，引用 `credentials(7)` /
`kill(2)` / `setreuid(2)`，每条决策都标注 Linux 出处。

本文档的目标：**把 reviewer 必查的项目，提前为 author 自查的清单**，
并把"对齐 Linux"从一个口头声明变成一份可验证的 artifact。

---

## 0. 触发判定

如果你的 PR 满足以下任一条，按本文档走：

- 修改 `os/StarryOS/kernel/src/syscall/` 下任意文件
- 修改 `components/starry-{process,signal}/` 公共 API
- 修改任何与 Linux man-page 行为有对照关系的代码路径
- 改动涉及多线程 / 并发 / 锁序

否则按常规 `CONTRIBUTING.md` 走即可。

---

## 1. Spec-first：先写规范，再写代码

在动任何 Rust 代码前，在 PR 描述（或 RFC issue）里先写完本节四份产出物。

### 1.1 Source-of-truth 三级阶梯（必须爬到 L3）

只读 man = 必然漏边界。Alignment 声明必须爬到 L3。

| 层级 | 内容 | 局限 | 何时够用 |
|---|---|---|---|
| L1 — man-page（`execve(2)`） | 用户视角的 happy path | 错误码不全；并发/race 不提；Linux quirk 不写 | 调研阶段 |
| L2 — POSIX / `signal(7)` 类总览 | 跨实现可移植契约 | Linux 实际扩展不覆盖；和 glibc 包装层混淆 | 划清"可移植"边界时 |
| L3 — Linux 内核源码 + git log | 行为的最终真相 | 要时间、要工具 | **任何 alignment PR 必须** |

L3 不可省。PR body 不写出"我读了哪个 commit 的哪个文件的哪个函数"，
就视同没做功课。

### 1.2 Branch enumeration 表（强制 artifact）

打开对应内核文件（execve → `fs/exec.c`、信号 → `kernel/signal.c`、
futex → `kernel/futex/`），把入口函数到返回的**每一条 `if` /
`switch` / `goto err_*`** 抄成表，**填完再写代码**：

| # | Linux 分支 | 文件:行 @sha | 触发输入 | Linux 可观察结果 | 我方实现位置 | 我方测试 phase |
|---|---|---|---|---|---|---|
| 1 | `if (IS_ERR(filename)) return PTR_ERR(...)` | exec.c:2034 @abc1234 | path 拷贝失败 | EFAULT | execve.rs:42 | phase0a |
| 2 | `count_strings(argv) — argv==NULL → 0` | exec.c:count_strings | argv=NULL | 视为空列表，成功 | execve.rs:55 | phase0b |
| 3 | `de_thread` 调用 | exec.c:1145 | TGID ≠ caller TID | 非 leader 接管 leader TID | ops.rs:rebind_task_tid | phase2 |
| … | | | | | | |

**自查规则**：表里"我方实现"或"我方测试"列空着的行 = 已知未对齐项。
PR push 前这两列不能有空（已知未实现的，单独移到 1.3 清单 A 标注，
不要在表里留空）。

这张表既是给 reviewer 的对账单，也是给自己的施工清单。**写表的过程
本身就拦住 80% 的 alignment 错误**——PR #273 里"非 leader 直接 EPERM"、
"NULL argv → EFAULT"、"清空 pending signals"、"SIG_DFL → SIG_IGN
提升"全部是表里如果填过就不会写错的格子。

### 1.3 四张 ABI 边界清单

对每个修改的 syscall，PR body 里贴四张表：

**清单 A — 错误码穷举**

来源 = `man 2 <syscall>` 的 ERRORS 段 ∪ 内核源码 `grep -nE "return -E[A-Z]+"`。

```
EFAULT  — 任一 user pointer 拷贝失败                我方: ✅ execve.rs:38
ENOENT  — path 解析不到                            我方: ✅ via FS_CONTEXT
E2BIG   — argv+envp 总字节数 > MAX_ARG_STRLEN      我方: ❌ 未实现 (defer)
ENOEXEC — header 不识别                            我方: ✅ load_user_app
ETXTBSY — 目标文件被以 write 方式打开                我方: ❌ 未实现 (defer)
…
```

不实现没关系——**写明"未实现"就够了**。reviewer 不会因为已知 gap
拒绝，但会因为未声明的 gap 拒绝。

**清单 B — flag/mode 位**：每个 bit 的语义、互斥关系、未知 flag 的处理。

**清单 C — 特殊输入**：`NULL` / `0` / `""` / `"/"` / `INT_MAX` /
已 unmap 的指针 / 非对齐指针 / 越界 — Linux 每种下做什么、我们做什么。

**清单 D — 跨 syscall 状态相互作用**：本 syscall **保留 / 重置 / 继承**
哪些进程级状态。execve 这一类至少要列：signal mask、pending signals
(process+thread)、signal handlers、sigaltstack、fd table、CLOEXEC、
credentials、aspace、TLS、robust_list、clear_child_tid、posix timers、
tracer、parent death signal、controlling tty …

PR #273 漏 "execve 不应清空 pending signals" 就是因为没填这张表。

### 1.4 Fault enumeration（并发改动专属）

复用 §1.2 表，加并发维度：

| # | 我方关键路径状态 | Sibling 状态 | 期望行为 | Linux 对应路径 | 测试 |
|---|---|---|---|---|---|
| 1 | exec teardown 中 | 阻塞在 vfork wait | sibling 被唤醒并退出 | `de_thread` + `wake_up_task` | phase6 |
| 2 | exec teardown 中 | 持 robust list | 退出时设 OWNER_DIED | `handle_futex_death` | phase7 |
| 3 | exec teardown 中 | 并发 fcntl(F_SETFD,CLOEXEC) | 新设的 CLOEXEC fd 也被关闭 | `do_close_on_exec` | phase5 |
| 4 | 进入 exec_lock 等待 | 持锁者刚返 EINTR | 等待者应能拿到锁继续 | `cred_guard_mutex` 语义 | phase4 |
| … | | | | | |

并发改动至少要枚举：
- 同进程其他线程的并发系统调用（哪些可能停在哪些等待点）
- 不可中断 wait 路径（`WaitQueue::wait_until`、`PollSet`、原子上下文等）
- 共享资源（CLONE_VM / CLONE_SIGHAND / CLONE_FILES 的对端）
- 失败注入点（哪些操作有 fallible `?`、哪些是不可逆）

### 1.5 自查的具体动作（教你怎么干）

光说"读内核"太空。下面是七个**可立刻执行**的动作，按顺序做完一遍，
alignment 风险降一个数量级。

#### 动作 A — 本地拉一份 Linux 源码并锁 SHA
```bash
# 一次性
git clone --depth=10000 https://github.com/torvalds/linux ~/src/linux

# 每个 PR 开始时
cd ~/src/linux && git fetch && git rev-parse origin/master > /tmp/linux_sha
```
PR body 里写：`Linux reference: torvalds/linux@<sha>`。所有源码引用都按这个 SHA。

#### 动作 B — 读完源码再画表
打开对应入口函数，**从 `grep -n "return\|goto err"` 倒着读**：
每个 return / goto 就是 §1.2 表里一行。读完再开始写 Rust。

经验值：execve 类一级 syscall，读源码 + 填表 ≈ 2–4 小时。看起来慢，
但**省下了后面 7 轮 review 的来回**——PR #273 反复打了一个月，画表
4 小时绝对划算。

#### 动作 C — Linux/StarryOS 同二进制 diff
test-suit 里的 C 测试在 Linux 上能直接编译。**写完每个 phase 前**：
```bash
gcc -static main.c -o repro
./repro > linux.out 2>&1                    # 跑 Linux 拿基准
# 再跑 starry qemu 拿同样输出
diff linux.out starry.out                   # 必须为空
```
任何 diff 都是 alignment bug，不是"测试不严谨"。这条把"我以为 Linux
这样"换成"Linux 实测这样"。

#### 动作 D — LTP 对账
Linux Test Project 是事实标准。
```bash
git clone --depth=1 https://github.com/linux-test-project/ltp ~/src/ltp
ls ~/src/ltp/testcases/kernel/syscalls/execve/
# execve01.c execve02.c … execve06.c
```
他们写了几个 case、我们写了几个、覆盖比是多少。差距大说明 fault
enumeration 漏。LTP 的 case 经常移植成本很低（POSIX C 代码），值得直接
搬到 `test-suit/starryos/`。

#### 动作 E — 历史 bug 反向枚举
Linux 自己踩过的坑就是你的 fault 字典：
```bash
git -C ~/src/linux log --oneline --since=5.years.ago -- fs/exec.c \
    | grep -iE "fix|bug|race|leak|deadlock|use-after"
```
读最近 10–20 条。每条都是一个"曾经有人写错"的具体案例。PR #273 里的
CLOEXEC race、vfork 不可中断、robust list owner ID 用错——这三个在
Linux git log 里都能直接搜到对应 fix commit。**你能搜到，reviewer 也能搜到**。

#### 动作 F — strace 探针
对任何"Linux 真这样吗？"的怀疑，**写最小 C repro + strace**，不要猜：
```bash
cat > /tmp/q.c <<'EOF'
#include <unistd.h>
int main(){ return execve("/bin/true", NULL, NULL); }   // argv=NULL 真的能跑？
EOF
gcc /tmp/q.c -o /tmp/q && strace -e execve /tmp/q
# execve("/bin/true", NULL, NULL) = 0    ← 实证 NULL 合法
```
**禁止仅凭 LLM 回答下结论**。LLM 经常把 POSIX 理想和 Linux 现实混淆。
strace 是最终仲裁。

#### 动作 G — errno 矩阵硬编码进测试
对每个 syscall，把"输入 → errno"做成测试表（C 代码直接生成）：
```c
struct case_t { /*input*/ ...; int expected_errno; const char *why; } cases[] = {
    { .path = NULL,            .err = EFAULT,        .why = "NULL filename" },
    { .path = "",              .err = ENOENT,        .why = "empty path" },
    { .path = "/no/such",      .err = ENOENT,        .why = "missing" },
    { .path = "/etc/passwd",   .err = EACCES,        .why = "not executable" },
    { .path = TOOLONG,         .err = ENAMETOOLONG,  .why = "path > PATH_MAX" },
    /* … 穷举 §1.3 清单 A … */
};
for (size_t i = 0; i < N; ++i) { ...syscall...; assert(errno == cases[i].err); }
```
这段循环既是测试，又是 §1.3 清单 A 的可执行版本。**两者必须一一对应**。

### 1.6 Reviewer 阻塞点发现 drill

PR #273 的 review 历史显示，reviewer 不是靠"多跑几遍"发现阻塞点，
而是按固定模式做 fault hunting。author 在首版 PR 前必须自己跑一遍同样的
drill，并把产物放进 PR body 的「并发正确性与验证方式」小节。

#### Drill A — point-of-no-return 表

列出每个不可逆状态修改点，以及它后面是否还有 fallible 操作：

| # | 不可逆操作 | 之后仍可能失败的操作 | 失败后可观察状态 | 处理方式 | 测试 |
|---|---|---|---|---|---|
| 1 | kill/zap sibling threads | path/ELF/interpreter load | 原进程已被拆掉但 exec 失败 | 先 pin/load 完成，再进入 commit | phase1 |
| 2 | 替换地址空间 | 栈构造/auxv 写入 | old image 不可恢复 | 不可逆点后禁止 `?`，或转 fatal path | phase2 |
| 3 | close CLOEXEC fds | 新 image 仍可失败 | fd table 被提前改变 | close-on-exec 放到 commit 段 | phase3 |

自查问题：**从这个点往后，如果下一行 `?` 返回错误，用户会看到什么？**
答不上来就说明 commit point 还没定义清楚。

#### Drill B — 跨层 precondition 表

每个特殊输入都必须从 syscall 层追到最底层 helper，不能只看入口函数：

| 输入 | syscall 层语义 | 中间层表示 | 下游隐含前提 | 成功路径测试 | 失败路径测试 |
|---|---|---|---|---|---|
| `argv == NULL` | Linux 视为空 argv list | `Vec<String>` | stack 构造需要 `argv[0]` 用于 `AT_EXECFN` | `execve(existing, NULL, NULL)` | `execve(missing, NULL, NULL)` |
| `envp == NULL` | Linux 视为空 env list | empty slice | env iterator 可为空 | existing path | missing path |
| `path == NULL` | `EFAULT` | 不进入 loader | 无 | 不适用 | raw syscall |

PR #273 最后一轮 NULL argv bug 就是这里漏了：只测了 missing path 的
`ENOENT`，没有测 existing path，结果真正进入 stack 构造后才触发
`argv_slice[0]` panic。**特殊输入必须同时覆盖成功路径和失败路径。**

#### Drill C — sibling 状态清单

多线程 syscall 不能只测 sibling 在用户态自旋的 happy path。至少枚举：

| sibling 状态 | 可能卡住的位置 | exec/exit 期望动作 | 需要的唤醒/中断机制 | 测试 |
|---|---|---|---|---|
| 正在用户态运行 | timer/preempt 后进内核 | 收到 internal zap 并退出 | task wakeup | phaseA |
| 阻塞在 vfork wait | `WaitQueue::wait_until` | 被唤醒并消费 exit request | 显式 wake wait queue | phaseB |
| 持有 robust futex | futex owner = `gettid()` | 退出时设置 OWNER_DIED | `handle_futex_death(Thread::tid)` | phaseC |
| 正在改 fd flag | `fcntl(F_SETFD)` | CLOEXEC 与 exec snapshot 一致 | fd table 锁或 teardown 后 snapshot | phaseD |
| 并发执行 execve | `exec_lock` | 等待 holder 结束后重试/继续 | 阻塞锁，不用 `try_lock` 误报 | phaseE |

自查规则：清单中每一行都要能回答"它现在睡在哪个 wait queue / 持哪个锁 /
谁负责叫醒它"。如果只能说"应该会退出"，说明还没审完。

#### Drill D — race window 表

所有 snapshot 都要写出保护条件：

| 共享状态 | snapshot 时间 | 并发修改者 | 错误交错 | 保护方式 | 测试 |
|---|---|---|---|---|---|
| fd table CLOEXEC 位 | sibling teardown 前 | sibling `fcntl` | snapshot 后设置 CLOEXEC，fd 泄漏进新 image | teardown 后 snapshot，或持同一把锁覆盖修改 | phaseD |
| thread list | zap sibling 中 | fork/clone/exit | 新 sibling 漏杀或重复释放 | exec lock 覆盖 thread group mutation | phaseE |
| signal pending set | exec commit 中 | signal delivery | 错误清空 pending signal | 按 Linux 保留规则处理 | phaseF |

reviewer 在 5 月 12 日指出的 CLOEXEC race 和 `exec_lock.try_lock()` 问题，
本质上都是这张表能提前暴露的窗口。

#### Drill E — 新 invariant 传播审计

只要 PR 引入新的语义区分，就必须 grep 全仓旧用法并在 PR body 贴结果。

| 新 invariant | 旧写法 | grep 命令 | 每个命中的处理 | 测试 |
|---|---|---|---|---|
| user-visible TID 可不同于 scheduler task id | `current().id()` | `rg "current\\(\\)\\.id\\(|\\.id\\(\\)" kernel components` | 判断需要 scheduler id 还是 `Thread::tid()` | robust futex phase |
| explicit `SIG_IGN` 不等于 default-ignore | `is_ignore(signo)` | `rg "is_ignore|SIG_IGN|SIG_DFL"` | 只保留用户显式设置的 ignore | signal exec phase |

PR #273 的 robust futex bug 就是因为 `Thread::tid()` 与 scheduler id 分离后，
`handle_futex_death()` 仍用旧 ID。**新 invariant 不做 grep 审计，等于没实现完。**

#### Drill F — 锁内副作用审计

持锁代码块里禁止调用可能唤醒 waiter、drop 大对象、释放 fd/file lock、
触发调度或进入用户内存访问的函数。写成表：

| 锁 | 锁内调用 | 可能副作用 | 修正方式 |
|---|---|---|---|
| `FD_TABLE.write()` | `release_locks_on_close(file)` | 唤醒 waiter / drop descriptor / 间接重入 | 锁内只收集 fd 和 file，解锁后释放 |
| process thread list lock | wake sibling | 调度/重入 | 先收集 task handle，解锁后 wake |

这类问题通常不靠测试稳定复现，但 reviewer 会按 lock side-effect 直接审出来。

#### Drill G — 测试有效性审计

每个新增 case 在 PR body 贴三条证据：

```text
discover: cargo xtask starry test qemu --arch <arch> -c <case> --list
success_regex: exactly one final marker, e.g. ALL_PHASES_OK
negative proof: 临时注入一个对应 bug 时，本 phase 会失败或超时
```

测试只存在于目录里不够；runner 必须发现它，regex 必须等到最终 marker，
并且每个 phase 必须能证明自己会抓住对应 fault。

---

## 2. Test-first：先写测试，再实现

### 2.1 一个 phase 锁一个 fault
不要把多线程逻辑塞进单一测试。按 §1.4 fault enumeration 拆 phase，
每个 phase 独立验证一个 invariant，phase 之间用 marker 隔开。

### 2.2 success_regex 必须是**单一终态 marker**

反面教材（PR #273 早期版本）：

```toml
success_regex = "PHASE0_OK|PHASE1_OK|PHASE2_OK|..."
```

runner 匹到第一个就判通过 → 后面所有 phase 没跑也算成功。**绿 CI 变假信号**。

正确做法：

```toml
success_regex = "ALL_PHASES_OK"
```

只有最后一个 phase 通过才输出 `ALL_PHASES_OK`，之前任何 phase 失败都阻塞。

### 2.3 测试目录放对位置
新增 `test-suit/starryos/normal/...` case 时必须验证 runner 能发现：

```bash
cargo xtask starry test qemu --arch <arch> -c <case-name> --list
```

如果输出 `unknown Starry normal test case`，说明放错位置，需要放到带
`build-*.toml` 的 wrapper（如 `normal/qemu-smp1/`）下。

### 2.4 跨四架构都要建测
不要只在本机最熟的架构上跑。每个新增 C 测试至少要通过 cross-build：

```bash
for arch in aarch64 riscv64 x86_64 loongarch64; do
    cargo xtask starry build --arch $arch
done
```

特别关注 musl 跨编译工具链的 UAPI header 可用性。可移植代码不要包含
`<linux/*.h>`——loongarch64-linux-musl-cross 不带这些头；需要的常量本地
`#ifndef` / `#define`，遵循 `test-suit/.../syscall/test-futex-robust-list/`
的写法。

### 2.5 C 测试代码风格
- 不使用 `__pass` / `__fail` 等保留标识符（双下划线前缀 C 标准保留给实现）
- 单语句 `for`/`if`/`while` 加大括号
- `test_framework.h` 跨 case 复制时保持命名一致

---

## 3. Scope discipline

### 3.1 一个 PR = 一件事
工作流变更、workspace-wide 配置、CI 重试 chore，**绝对不允许**混进 feature
PR。每次想加无关改动时问自己：

> 这一行如果被 revert，会不会影响这个 PR 的核心目标？

如果答案是「不会」，立即拆出去单独发 PR。

### 3.2 顶层文件触碰红线
以下文件的改动必须独立成 PR，禁止与功能开发混搭：
- 根目录 `Cargo.toml`（特别是 `default-members`、`workspace`）
- `.github/workflows/`、`scripts/`、`xtask/` 的非功能性改动
- `rust-toolchain.toml`、根 `rustfmt.toml`

PR #273 早期把根 `Cargo.toml` 的 `default-members` 改成只剩
`os/StarryOS/starryos`，影响整仓 cargo 默认行为——这是典型的红线越界，
直接被驳回，浪费一整轮 review。

---

## 4. Pre-push 自查清单

每次 push 前完整走一遍，当 git hook 用。

### 4.1 格式与基础静态检查
```bash
cargo fmt --all -- --check
git diff --check
```

### 4.2 受影响 crate 的 clippy
```bash
cargo xtask clippy --since <merge-base>
```

### 4.3 四架构 cross-build
对动了 syscall 或新增 C 测试的 PR：
```bash
cargo xtask starry build --arch aarch64
cargo xtask starry build --arch riscv64
cargo xtask starry build --arch x86_64
cargo xtask starry build --arch loongarch64
```

### 4.4 测试可发现 + 实际跑
```bash
cargo xtask starry test qemu --arch <arch> -c <new-case> --list   # 必须列出
cargo xtask starry test qemu --arch <arch> -c <new-case>          # 必须通过
```

### 4.5 PR body 与代码一致性
最后一次 push 后**重读 body 全文**，逐条对照 commit：
- 每个声称已实现的行为 → 找到对应代码位置
- 每个声称的验证结果 → 重新跑一遍贴最新输出
- 早期 TODO / 已知问题段落 → 删除或更新

PR #273 在 reviewer 第 N 轮才指出："body 声称 `handle_futex_death` 用
`Thread::tid` 但代码用 scheduler id"。**body 漂移让 reviewer 不得不带着
"body 可能在撒谎"的怀疑做每一轮 review，信任成本极高**。

### 4.6 Linux 语义自查（针对 syscall 类改动）

在 push 前对每个修改的 syscall 问自己：

- [ ] §1.2 branch table 每行的"我方实现"和"我方测试"两列都填了
- [ ] §1.3 四张清单都已贴在 PR body，"未实现"项明确标注
- [ ] §1.4 fault enumeration 每行都有对应 phase
- [ ] §1.6 reviewer drill 已跑完，point-of-no-return / precondition /
      race window / invariant audit 表都已贴在 PR body
- [ ] 失败路径上没有不可逆副作用（不可逆点之后所有 `?` 都已审视）
- [ ] 特殊输入同时覆盖成功路径和失败路径（例如 `argv == NULL` 既测
      missing path，也测 existing executable path）
- [ ] syscall 层接受的表示没有传给下游更严格的隐含前提；如果有，已在
      loader/helper 层归一化或显式处理
- [ ] 所有 sibling wait 点都有 teardown 唤醒路径，尤其是不可中断 wait /
      vfork wait / futex wait
- [ ] 所有 snapshot 都写明了保护锁或 quiesce 顺序，不存在 snapshot 后
      sibling 继续修改共享状态的窗口
- [ ] 持锁期间没有调用可能唤醒 waiter / drop 大对象 / 触发副作用的函数
- [ ] 所有 ID 比较都用了正确的 ID 类型（`Thread::tid` vs `task.id()`）
- [ ] 新增或改变的 invariant 已用 `rg` 全仓审计旧用法，并在 PR body 贴结果
- [ ] CLONE_\* 共享资源在 detach 前被正确 COW
- [ ] 信号状态变更符合 `man 7 signal` 的保留/重置规则
- [ ] §1.5 动作 C（Linux/StarryOS 同二进制 diff）已跑且 diff 为空

---

## 5. PR 描述模板（强制）

PR body 必须含以下小节，缺一不可：

```markdown
## 问题描述
（一段，<200 字，说现状什么不工作）

## 根本原因分析
（引用 Linux man-page / kernel 源码，说明正确行为应是什么）

## 修复方案
（按文件分点列出关键改动，每条说"做了什么 + 为什么这样做"，
 引用 Linux 对应代码路径）

## 测试覆盖
（按 phase 列出，每个 phase 标注它锁定的 fault 来自 §1.4 哪一项）

## Linux 对齐证据
（见 §6）

## 并发正确性与验证方式
（见 §6.7；无并发影响也要说明为什么无共享状态/无 sibling 交互）

## 验证
（贴最新一次 push 后的 fmt / clippy / cross-build / qemu 输出）
```

PR #246（per-process credentials subsystem）是仓库内最贴近此模板的范例，
新作者建议先读那一份 PR body 再开自己的 PR。

---

## 6. PR body 强制产出：Linux 对齐证据段

PR 描述追加，缺一不可。这一段把"对齐 Linux"从声明变成可验证 artifact：

```markdown
## Linux 对齐证据

### 6.1 已读源码
- 引用 SHA: torvalds/linux@<sha>
- `fs/exec.c`: do_execveat_common, begin_new_exec, de_thread, flush_old_exec
- `kernel/signal.c`: flush_signal_handlers, flush_signals
- `fs/file.c`: do_close_on_exec
- `kernel/futex/core.c`: handle_futex_death

### 6.2 Branch enumeration 表
（§1.2 那张表完整贴出，每行都标注我方实现位置 + 测试 phase；
 "未实现"行单独列出并说明理由）

### 6.3 四张清单
（§1.3 A/B/C/D 完整贴出）

### 6.4 LTP 对账
- LTP 相关 case 数：N（路径：testcases/kernel/syscalls/execve/）
- 我方覆盖：M (M/N = X%)
- 未覆盖逐条说明：……

### 6.5 Linux ↔ StarryOS 同二进制 diff
- 测试二进制：test-mt-execve（commit <sha>）
- Linux 输出 hash: <sha256>
- StarryOS 输出 hash: <sha256>
- diff: 无 / 仅以下可解释差异：……

### 6.6 历史 bug 字典
读过的相关 Linux fix commit（至少 5 条），逐条说明我方如何规避：
- linux@<sha>: "exec: fix race in CLOEXEC" → 我方对应处理在 execve.rs:N
- ……

### 6.7 并发正确性与验证方式

#### point-of-no-return
（贴 §1.6 Drill A 表；每个不可逆点后是否还有 fallible 操作必须写清楚）

#### cross-layer precondition
（贴 §1.6 Drill B 表；每个特殊输入都覆盖 success path 和 error path）

#### sibling-state coverage
（贴 §1.6 Drill C 表；每个 sibling wait/lock 状态都有唤醒和退出方案）

#### race-window proof
（贴 §1.6 Drill D 表；每个 snapshot 都写明锁或 quiesce 顺序）

#### invariant propagation audit
（贴 §1.6 Drill E 表；列出 grep 命令、命中位置、每处处理结论）

#### lock side-effect audit
（贴 §1.6 Drill F 表；锁内无 wake/drop/release/reentrant side effect）

#### test effectiveness proof
- `--list` 输出证明 case 被 runner 发现
- `success_regex` 是单一最终 marker
- 每个 phase 对应一个 fault，且有 negative proof
```

reviewer 看到这一段就能机械地核对：每行表里都填了 → 真的查过；输出 diff
为空 → 实测对齐。**对齐从此是可验证的事实，不是声明**。

---

## 7. Review 回应纪律

收到 reviewer 评论后：

- 不要每条单独发一个 fix commit。同主题的修复合并到一个 commit，
  PR 历史更易读。
- 每条评论都要在线程里**明示**怎么处理：fixed in `<sha>` / won't fix
  because ... / deferred to follow-up PR。沉默处理是反模式。
- 修复 push 后同步更新 PR body 的「验证」小节，让 reviewer 看到最新状态。
- **同一类问题第二次被指出时**：停下来重读本文档 §1，问自己 spec-first
  步骤跳过了哪一项。第三次同类被指出意味着应该 self-revert 重做这一节
  而不是继续打补丁。

---

## 8. 反模式黑名单（一眼自检）

PR body 出现下列任何一条 = 没做功课，应当 self-revert 重做：

- ❌ "参照 Linux 实现" — 没引 SHA、文件、函数
- ❌ "符合 POSIX 语义" — 没引 IEEE Std 1003.1 章节
- ❌ "本地测试通过" — 没说测了哪些 phase / 同 Linux diff 结果
- ❌ "已修复 reviewer 提出的问题" — 没列具体 fault 来自 §1.4 哪一项
- ❌ "应该不会有并发问题" — 没填 §1.4 fault enumeration 表
- ❌ "并发正确性见实现" — 没贴 §6.7 point-of-no-return / race-window 表
- ❌ 只测特殊输入的失败路径 — 例如 `argv == NULL` 只测 missing path，
  没测 existing executable path
- ❌ 引入新 ID / 状态 invariant 后没贴 grep 审计 — 例如 `Thread::tid`
  和 scheduler id 分离后没审 `current().id()`
- ❌ `success_regex` 含多个 `|` — 见 §2.2
- ❌ 一个 PR 里同时包含 feature + 工具链/workspace 配置改动 — 见 §3
- ❌ PR push 后 body 未同步更新 — 见 §4.5
- ❌ 同一类 reviewer 评论出现 ≥3 次 — 见 §7

---

## 9. 案例对照：PR #273 vs PR #246

| 维度 | PR #273（反例） | PR #246（正例） |
|---|---|---|
| commits | 28（含 11 个 "address review" 补丁） | 4 |
| review 轮次 | 21 | 2 |
| 时间跨度 | ≈ 1 个月 | 几天 |
| PR body 开篇 | "## Summary" + 实现细节 | "## 问题描述" + "## 根本原因分析" |
| Linux 引用密度 | body 中 0 个 commit SHA / 函数名 | 每个权限规则都标注 `kill(2)`/`credentials(7)`/`setreuid(2)` |
| 测试同步 | 初版无测试，事后补 | 与实现同 PR 提交，列出每个用例覆盖 |
| Scope | 含根 `Cargo.toml` 改动 + CI chore | 单一主题 |
| 跨架构验证 | loongarch musl 失败到 reviewer approve 后才暴露 | 提交前已通过 |

读 PR #246 的 body 是开任何 Linux 语义 PR 之前的最低准备动作。
