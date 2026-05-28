```markdown
# StarryOS PR 代码修复 — AI 协作文档

## 项目背景

当前 PR：`pr/nsproxy-core`，将 6 种 Linux namespace 的骨架引入 StarryOS。
目标：修复代码评审中发现的阻塞问题，使 namespace 隔离真正生效。

## 架构概览

- **axnsproxy crate**：`os/StarryOS/axnsproxy/`，统一管理所有 namespace 类型
  - `src/lib.rs`：`NsProxy` 聚合器，`clone_all()`、`unshare_xxx()` 方法
  - `src/uts.rs`、`src/pid.rs`、`src/mnt.rs`、`src/net.rs`、`src/ipc.rs`、`src/user.rs`
- **ProcessData**：`os/StarryOS/kernel/src/task/mod.rs`，通过 `nsproxy: SpinNoIrq<axnsproxy::NsProxy>` 字段访问
- **clone 路径**：`os/StarryOS/kernel/src/syscall/task/clone.rs`
- **IPC 模块**：`os/StarryOS/kernel/src/syscall/ipc/shm.rs`、`msg.rs`
- **网络路径**：`os/StarryOS/kernel/src/file/net.rs`、`netlink.rs`、`packet.rs`

---

## 🔴 阻塞问题 1：fork/clone 未继承父进程 nsproxy

### 问题描述

`ProcessData::new()` 始终调用 `NsProxy::new_root()` 创建全新的 root namespace。在 `clone.rs` 的 fork 路径中，子进程创建后**没有**从父进程拷贝 `nsproxy`，导致：
- 每个子进程都拿到 root namespace
- 父进程通过 `unshare` 创建的隔离完全丢失
- `NsProxy::clone_all()` 方法已定义但**从未被调用**

### Linux 标准行为

- `fork()` / `clone()` 不带 `CLONE_NEW*` 标志 → 子进程**共享**父进程的所有 namespace
- `clone(CLONE_NEWXXX)` → 子进程在对应 namespace 上创建新实例，其他 namespace 继承父进程

### 修复目标

修改 `clone.rs` 的 `do_clone` 函数，在创建子进程后：
1. 如果没有任何 `CLONE_NEW*` 标志 → 调用 `parent_nsproxy.clone_all()` 赋值给子进程
2. 如果有 `CLONE_NEWIPC` → 在 `clone_all()` 基础上再调用 `unshare_ipc()`
3. 同理处理 `CLONE_NEWNET`、`CLONE_NEWPID`、`CLONE_NEWUTS`、`CLONE_NEWNS`、`CLONE_NEWUSER`

### 涉及文件

- `os/StarryOS/kernel/src/syscall/task/clone.rs`：`do_clone` 函数
- `os/StarryOS/kernel/src/task/mod.rs`：`ProcessData::new()`（可能需要添加 `with_nsproxy` 构造方法）

### 修改要求

1. 在 `clone.rs` 中找到创建子进程 `ProcessData` 的位置
2. 添加从父进程继承 `nsproxy` 的逻辑
3. 使用 `NsProxy::clone_all()` 和 `NsProxy::unshare_xxx()` 方法
4. 不要修改 `ProcessData::new()` 的默认行为（保持向后兼容）

---

## 🔴 阻塞问题 2：key_shmid 全局映射跨 namespace 覆盖

### 问题描述

`shm_manager.insert_key_shmid(key, shmid)` 使用全局 `BiBTreeMap<key, shmid>`。当不同 IPC namespace 使用相同 `key` 创建共享内存段时，后创建的 namespace 会覆盖先创建的 namespace 的映射。

**示例**：
```
namespace A: key=42 → shmid=1
namespace B: key=42 → shmid=2
// 此时全局映射中 key=42 → shmid=2，namespace A 的查找丢失
```

### 修复目标

将 key 改为 `(key, ns_id)` 复合键，使不同 namespace 的同名 key 互不干扰。

### 涉及文件

- `os/StarryOS/kernel/src/syscall/ipc/shm.rs`：`insert_key_shmid`、`find_shmid_by_key` 及相关调用
- `os/StarryOS/kernel/src/syscall/ipc/msg.rs`：如果消息队列有类似问题，同步修复

### 修改要求

1. 找到 `insert_key_shmid` 和 `find_shmid_by_key` 的定义
2. 将 key 类型从 `i32` 改为 `(i32, u64)`（key + ns_id）
3. 更新所有调用点，传入当前进程的 `ns_id`
4. 确保 IPC_INFO / SHM_INFO 等全局查询命令仍正确返回**当前 namespace** 的信息

---

## 🟡 问题 3：in_root_net_ns() 重复定义

### 问题描述

`in_root_net_ns()` 函数在以下 3 个文件中完全相同地定义了 3 次：
- `os/StarryOS/kernel/src/file/net.rs`
- `os/StarryOS/kernel/src/file/netlink.rs`
- `os/StarryOS/kernel/src/file/packet.rs`

### 修复目标

提取到一个共享位置（如 `crate::ns` 模块或 `axnsproxy` 中），消除重复。

### 涉及文件

- 上述 3 个文件：删除重复定义，改为 `use` 导入
- 新建共享位置：`os/StarryOS/kernel/src/ns.rs` 或放入 `axnsproxy/src/lib.rs`

### 修改要求

1. 选择或创建一个合适的共享模块
2. 将 `in_root_net_ns()` 移入该模块
3. 在 3 个原文件中用 `use` 导入替换

---

## 🟡 问题 4：magic number 替换为命名常量

### 问题描述

`shm.rs` 中 `if cmd == 14` 使用了 magic number，而同文件中 `IPC_INFO` 已经使用了命名常量。

### 修复目标

添加 `const SHM_INFO: i32 = 14;` 并替换 magic number。

### 涉及文件

- `os/StarryOS/kernel/src/syscall/ipc/shm.rs`

---

## 代码修改规范

### 通用要求

1. **每次只修改一个问题的代码**，不混入无关改动
2. **修改前先展示当前代码**（用 `grep` 或 `cat` 定位目标行），修改后给出完整 diff
3. **每个修改点说明**：在哪个文件、哪个位置、改了什么、为什么这样改
4. **保持代码风格与现有代码一致**
5. **提交前必须通过 `cargo fmt --all` 检查**

### 系统调用相关

- 所有 namespace 操作统一通过 `current().as_thread().proc_data.nsproxy.lock()` 访问
- 使用 `NsProxy` 的已有方法（`clone_all()`、`unshare_xxx()`），不在系统调用中直接操作底层字段

### 错误处理

- 不支持的 flag 组合返回 `EINVAL` + `warn!` 日志
- 权限检查返回 `EPERM`
- 资源分配失败返回 `ENOMEM`，不 panic

---

## Git 规范

- 分支：当前在 `pr/nsproxy-core`
- Commit：`fix: <描述>`
- 每次修复推送到 `myfork/pr/nsproxy-core`

---

## 验证要求

每个问题修复后，用以下命令验证：

### 问题 1 验证（fork 继承 namespace）

```bash
# 在 StarryOS 中执行
unshare --ipc /bin/sh -c "ipcs -m"
# 预期：创建共享内存后在子进程命名空间中可见，退出后不影响父命名空间
```

### 问题 2 验证（key 隔离）

```bash
# 两个不同 namespace 使用相同 key 创建共享内存
# 预期：互不冲突，各自独立
```

### 问题 3 验证

```bash
cargo clippy --package starry-kernel -- -D warnings
# 预期：无重复代码警告
```

---

## 环境信息

- 内核：StarryOS（Rust），QEMU aarch64
- 用户态：Debian GNU/Linux 13 (trixie)
- 源码路径：`~/桌面/tgoskits/`
- axnsproxy：`os/StarryOS/axnsproxy/src/`
- syscall：`os/StarryOS/kernel/src/syscall/`

---

## 当前任务

按优先级修复以下问题：
1. 🔴 fork/clone 继承父进程 nsproxy（clone.rs）
2. 🔴 key_shmid 使用 (key, ns_id) 复合键（shm.rs）
3. 🟡 in_root_net_ns() 提取到共享模块
4. 🟡 SHM_INFO magic number 替换为常量
```