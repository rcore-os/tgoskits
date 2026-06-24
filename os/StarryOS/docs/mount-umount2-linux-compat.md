# StarryOS `mount` / `umount2` Linux 兼容实现说明

本文档说明本次在 StarryOS 中围绕 `sys_mount()` / `sys_umount2()` 做的 Linux 兼容性补齐，重点覆盖：

- `mount(2)` 中的 propagation flags 组合校验
- `MS_SHARED` / `MS_PRIVATE` / `MS_SLAVE` / `MS_UNBINDABLE`
- `MS_BIND` / `MS_BIND|MS_REC`
- bind 子目录
- `MS_MOVE`
- `MS_MOVE` 对 descendant target 的 `ELOOP`
- `MS_REMOUNT`
- `MS_RDONLY`
- `MS_REMOUNT|MS_BIND|MS_RDONLY`
- `umount2(2)` 中的 `MNT_EXPIRE`
- `UMOUNT_NOFOLLOW`
- `MNT_DETACH`

本文档分成两层来讲：

1. Linux 语义本身应该是什么
2. StarryOS 这次是如何把这些语义落到现有 VFS / mount tree 上的

## 1. 背景：`mount(2)` 不是单一语义

Linux 的 `mount(2)` 并不是“永远把一个文件系统挂到某个目录”这么简单。

从 `mountflags` 来看，它至少有几类完全不同的操作：

- 普通挂载：
  不带 `MS_BIND` / `MS_MOVE` / `MS_REMOUNT` / propagation flags 时，表示“把一个新的 filesystem 挂到 target”
- propagation 操作：
  带 `MS_SHARED` / `MS_PRIVATE` / `MS_SLAVE` / `MS_UNBINDABLE` 时，表示“修改已有 mount 的传播属性”
- bind 挂载：
  带 `MS_BIND` 时，表示“把已有路径对应的 mount tree 映射到另一个位置”
- move 挂载：
  带 `MS_MOVE` 时，表示“把已有 mount 从一个挂载点移动到另一个挂载点”
- remount：
  带 `MS_REMOUNT` 时，表示“修改已有 mount 的 mount options，而不是新建 mount”

因此，内核实现不能把所有 `mount(2)` 调用都当成“新建一个普通挂载”。  
如果这样做，很多调用会“返回成功但语义错误”，这是本次修复前 StarryOS 的主要问题。

## 2. `sys_mount` 参数语义

Linux `mount(2)` 原型：

```c
int mount(const char *source,
          const char *target,
          const char *filesystemtype,
          unsigned long mountflags,
          const void *data);
```

五个参数在不同 `mountflags` 组合下，语义并不完全相同：

| 场景 | `source` | `target` | `filesystemtype` | `mountflags` | `data` |
|---|---|---|---|---|---|
| 普通挂载 | 文件系统源或设备 | 挂载点 | 文件系统类型 | 普通 mount flags | 文件系统专属参数 |
| propagation 操作 | 常被忽略或为 `"none"` | 已存在 mount | 通常无实际作用 | `MS_SHARED`/`MS_PRIVATE`/`MS_SLAVE`/`MS_UNBINDABLE` | 通常无作用 |
| bind 挂载 | 源路径 | 目标路径 | 常被忽略 | `MS_BIND` 或 `MS_BIND|MS_REC` | 无作用 |
| move 挂载 | 源挂载点 | 目标挂载点 | 常被忽略 | `MS_MOVE` | 无作用 |
| remount | 常被忽略或与原 mount 一致 | 已存在 mount | 常被忽略 | `MS_REMOUNT` 及其附加位 | 一般无作用或文件系统专属 |

这也是为什么实现 `sys_mount()` 时，第一步必须先按 `flags` 判定“这是哪一类操作”，而不能先看 `fs_type` 再决定逻辑。

## 3. Linux `mount(2)` 语义矩阵

### 3.1 propagation flags

Linux 中：

- `MS_SHARED`
- `MS_PRIVATE`
- `MS_SLAVE`
- `MS_UNBINDABLE`

这四个是 propagation type flags。

#### 合法性规则

| 组合 | Linux 预期结果 |
|---|---|
| 同时出现多个 propagation type flags | `EINVAL` |
| propagation flag 与 `MS_REC`/`MS_SILENT` 之外的 flag 混用 | `EINVAL` |
| propagation flag 单独使用 | 成功，修改已有 mount 的传播属性 |
| propagation flag + `MS_REC` | 成功，递归修改当前 mount tree |

#### 本次 StarryOS 的实现边界

这次 StarryOS 已经实现到“可被用户态测试验证”的 shared subtree 语义：

- propagation flags 非法组合检查
- `MS_SHARED`：shared mount 的 bind peer 进入同一传播组
- `MS_PRIVATE`：从 shared/slave 关系中脱离，停止后续传播
- `MS_SLAVE`：接收 master 的传播，但不向 master 反向传播
- `MS_UNBINDABLE`：禁止直接 bind，并在 recursive bind 时剪掉 unbindable child mount
- shared mount 下新增 child mount 时，对 peer / slave 做传播

仍未完全实现的，是 Linux mount namespace 更细的传播细节，例如：

- 更完整的 peer group 生命周期
- 更复杂的递归 propagation corner case
- namespace 之间更深层的传播隔离规则

### 3.2 `MS_BIND`

#### Linux 语义

`MS_BIND` 表示把已有路径处的 mount view 再映射到另一个路径。

最核心的行为：

- 目标路径能看到源路径当前看到的内容
- 对目标路径的修改，会作用到同一份底层对象
- 默认 **不递归复制 submount**

也就是说，普通 bind 的目标树：

- 顶层目录内容应该一致
- 但 source 树下面如果有更深一层的子挂载点，默认不应该自动出现在 bind 目标里
- source 不必是 mount root，也可以是 mount 内部的普通子目录

#### `MS_BIND|MS_REC`

如果同时带 `MS_REC`，则变成 recursive bind：

- 顶层 mount tree 被 bind
- nested submount 也要在目标树下可见

#### 结果矩阵

| flags | Linux 预期结果 |
|---|---|
| `MS_BIND` | 普通 bind，不带 nested submount |
| `MS_BIND|MS_REC` | 递归 bind，nested submount 也可见 |
| `MS_BIND|MS_RDONLY` | 新的 bind mount 视图只读 |
| `MS_REMOUNT|MS_BIND|MS_RDONLY` | 把已有 bind mount remount 成只读 |
| `MS_BIND` on unbindable source | `EINVAL` |

### 3.3 `MS_MOVE`

`MS_MOVE` 表示把一个已存在 mount 从旧挂载点移动到新挂载点。

#### Linux 结果

| 结果点 | Linux 预期结果 |
|---|---|
| 新路径 | 应看到原 mount 的内容 |
| 旧路径 | 不再是那个 mount 的可见入口 |
| mount 本身 | 不是复制，而是移动 |
| target 是 source 后代 | `ELOOP` |

这类操作的关键不是“新建一个 mount”，而是“修改 mount tree 中 parent-child 关系”。

### 3.4 `MS_REMOUNT`

`MS_REMOUNT` 表示修改已有 mount 的 flags，而不是新建一个文件系统。

最基本的语义要求：

- remount 之后，原有文件内容仍可见
- remount 改动作用于已有 mountpoint

#### 常见组合

| flags | Linux 预期结果 |
|---|---|
| `MS_REMOUNT` | 成功，内容保留 |
| `MS_REMOUNT|MS_RDONLY` | 把现有 mount 变成只读 |
| `MS_REMOUNT|MS_BIND|MS_RDONLY` | 把已有 bind mount 视图变成只读，而不是把 source mount 一起变只读 |

### 3.5 `MS_RDONLY`

`MS_RDONLY` 可以出现在两类场景：

- 普通挂载：创建一个只读 mount
- remount：把一个已有 mount 改成只读

#### Linux 结果

| 场景 | Linux 预期结果 |
|---|---|
| `mount(..., MS_RDONLY, ...)` | 挂载成功，但写入返回 `EROFS` |
| `mount(..., MS_REMOUNT|MS_RDONLY, ...)` | remount 成功，已有内容保留，后续写入 `EROFS` |
| `mount(..., MS_REMOUNT|MS_BIND|MS_RDONLY, ...)` | bind mount 视图只读，但源 mount 仍可写 |

除了普通 `write()`，Linux 风格的只读 mount 还应拒绝：

- `chmod`
- `rename`
- `unlink`
- `mkdir`
- 已打开 fd 的后续 `write` / `append`

## 4. Linux `umount2(2)` 语义矩阵

Linux `umount2()` 原型：

```c
int umount2(const char *target, int flags);
```

### 4.1 非法 flags 与非法组合

| flags | Linux 预期结果 |
|---|---|
| 含未知位 | `EINVAL` |
| `MNT_EXPIRE` 和 `MNT_FORCE` 同时出现 | `EINVAL` |
| `MNT_EXPIRE` 和 `MNT_DETACH` 同时出现 | `EINVAL` |

### 4.2 非挂载点

如果 `target` 不是 mount root，Linux 预期返回：

- `EINVAL`

### 4.3 `MNT_EXPIRE`

`MNT_EXPIRE` 是两阶段语义：

| 调用次数 | Linux 预期结果 |
|---|---|
| 第一次 | 标记为 expired，并返回 `EAGAIN` |
| 第二次 | 如果还是同一个未忙 mount，则真正卸载 |

这里的重点不是“第一次就卸载”，而是“第一次只打标记”。

### 4.4 `UMOUNT_NOFOLLOW`

带 `UMOUNT_NOFOLLOW` 时：

- `target` 如果是 symlink，不应跟随它
- 因而不会卸掉 symlink 指向的真实 mount

如果解析后目标不是 mount root，通常表现为：

- `EINVAL`

### 4.5 `MNT_DETACH`

`MNT_DETACH` 是 lazy unmount。

其关键语义：

- 即使 mount 当前 busy，也可以成功
- mount 从路径可见性上被摘掉
- 已经打开的 fd 还能继续访问原对象
- 新路径查找不再看到这个 mount

这与普通 `umount()` 的差别很大：

- 普通卸载：busy 时 `EBUSY`
- lazy detach：busy 也允许成功，但只是从 namespace 中脱离

## 5. StarryOS 本次实现思路

## 5.1 `sys_mount()`：先按操作类型分派，再决定具体行为

本次实现的首要改动是：`sys_mount()` 不再把所有调用都当成“新建普通挂载”。

核心结构如下：

```rust
if propagation != 0 {
    ...
    return Ok(0);
}

if (flags & MS_REMOUNT) != 0 {
    ...
    return Ok(0);
}

if (flags & MS_MOVE) != 0 {
    ...
    return Ok(0);
}

if (flags & MS_BIND) != 0 {
    ...
    return Ok(0);
}

match fs_type.as_str() {
    "tmpfs" => ...
    "ext4" => ...
    _ => ...
}
```

对应代码位置：

- `os/StarryOS/kernel/src/syscall/fs/mount.rs`

### 讲解

- propagation-only 调用要在最前面处理，因为它们不是“新挂载”
- remount 也必须早处理，否则会误走 bind 或普通 mount 路径
- `MS_MOVE` / `MS_BIND` 都属于 mount tree 操作，不应该落进 `tmpfs` / `ext4` 分支
- 只有当这些“特殊 mount 操作”都不命中时，才说明这是普通挂载

## 5.2 propagation flags：先校验合法性，再按 mountpoint 状态更新传播关系

本次实现：

```rust
let propagation = flags & PROPAGATION_FLAGS;

if propagation.count_ones() > 1 {
    return Err(AxError::InvalidInput);
}

if propagation != 0 {
    let allowed = propagation | MS_REC | MS_SILENT;
    if flags & !allowed != 0 {
        return Err(AxError::InvalidInput);
    }

    let target = FS_CONTEXT.lock().resolve(target)?;
    if !target.is_root_of_mount() {
        return Err(AxError::InvalidInput);
    }
    let mountpoint = target.mountpoint();
    if (propagation & MS_PRIVATE) != 0 {
        mountpoint.set_private();
    } else if (propagation & MS_SHARED) != 0 {
        mountpoint.set_shared();
    } else if (propagation & MS_SLAVE) != 0 {
        mountpoint.set_slave();
    } else if (propagation & MS_UNBINDABLE) != 0 {
        mountpoint.set_unbindable();
    }
    return Ok(0);
}
```

### 讲解

- `count_ones() > 1` 对应 Linux 的“多个 propagation type flags 同时出现是 `EINVAL`”
- `flags & !allowed != 0` 对应 Linux 的“propagation type flags 只能和 `MS_REC` / `MS_SILENT` 共存”
- propagation-only 调用仍然不新建 fs，而是修改已有 mountpoint 的传播属性
- 传播状态现在是 mountpoint 级别的，而不是底层 inode 级别的

## 5.3 `axfs-ng-vfs::Mountpoint`：把 mountpoint 当成 mount 语义状态承载体

本次把几个 Linux mount 语义相关的状态放到了 `Mountpoint` 上：

```rust
pub struct Mountpoint {
    root: DirEntry,
    location: Mutex<Option<Location>>,
    children: Mutex<HashMap<ReferenceKey, Weak<Self>>>,
    device: u64,
    readonly: AtomicBool,
    expired: AtomicBool,
    propagation: Mutex<PropagationState>,
}
```

对应代码位置：

- `components/axfs-ng-vfs/src/mount.rs`

### 为什么放在 `Mountpoint` 上

Linux 里的这些语义，本质上都是“对 mount 实例”的属性，而不是对底层 inode 或 filesystem 超级块的属性：

- bind mount 视图只读，不应必然把 source mount 一起变只读
- `MNT_EXPIRE` 的 expired 标记属于 mount，不属于底层文件
- lazy detach 也是从 mount tree 摘除，而不是销毁底层文件对象
- shared/private/slave/unbindable 也属于 mount 之间的关系，而不是目录项自己的关系

因此在 StarryOS 里，把这类状态绑定到 `Mountpoint` 是合理且稳定的。

## 5.4 bind / recursive bind：通过 child mount map 区分是否递归

本次 `MS_BIND` 和 `MS_BIND|MS_REC` 的核心差异，落在 `children` 这张表上。

实现片段：

```rust
fn bind(source: &Location, location_in_parent: Location, recursive: bool) -> Arc<Self> {
    let result = Self::new_with_root(
        source.entry.clone(),
        Some(location_in_parent),
        source.mountpoint.device(),
    );
    result
        .readonly
        .store(source.mountpoint.is_readonly(), Ordering::Release);
    if recursive {
        for (key, child) in source.mountpoint.children.lock().iter() {
            result.children.lock().insert(key.clone(), child.clone());
        }
    }
    result
}
```

### 讲解

- 顶层 bind 的本质是：目标 mountpoint 的 root 指向与 source 当前可见的目录入口
- source 可以是 mount 内部的普通子目录，不必是 mount root
- 普通 bind 时，不拷贝 `children`
  - 所以 nested submount 在目标树下不可见
- recursive bind 时，把 `children` 也带过去
  - 所以 nested submount 在目标树下继续可见
- 如果 source mount 被标记为 unbindable：
  - 直接 bind 返回 `EINVAL`
  - recursive bind 时会跳过 unbindable child mount

这正对应 Linux 中：

- `MS_BIND` 不递归
- `MS_BIND|MS_REC` 递归

## 5.5 `resolve_mountpoint()`：按当前 mount tree 的 child map 决定是否跨入子挂载

这是本次修复 bind 行为时最关键的一点。

实现片段：

```rust
fn resolve_mountpoint(self) -> Self {
    let Some(mountpoint) = self
        .mountpoint
        .children
        .lock()
        .get(&self.entry.key())
        .and_then(Weak::upgrade)
    else {
        return self;
    };
    let mountpoint = mountpoint.effective_mountpoint();
    let entry = mountpoint.root.clone();
    Self::new(mountpoint, entry)
}
```

### 讲解

如果路径解析只看“目录节点上有没有 mountpoint”，那么普通 bind 会错误地把 source 的 nested mount 一起暴露出来。  
本次改成“看当前 mount tree 的 `children` map 里有没有这个 child mount”，这样：

- 普通 bind：目标 mountpoint 没带 `children`，就不会跨进 nested mount
- recursive bind：目标 mountpoint 带了 `children`，就能看到 nested mount

这让“是否递归”真正体现在 mount tree 本身，而不是体现在共享的底层目录节点上。

同样的机制也被用在 shared/slave propagation 上：

- shared mount 新增 child mount 时
- 先计算 child 相对 mount root 的路径
- 再在 peer / slave 那边找到同一路径位置
- 最后把 child mount 暴露到对端对应的 `children` map 中

这样 peer 看到的是“对端树中相同相对路径上的 child mount”，而不是错误地复用源侧目录项位置。

## 5.6 `MS_MOVE`：修改 mount tree 的 parent-child 关系

实现片段：

```rust
pub fn move_to(self: &Arc<Self>, new_location: &Location) -> VfsResult<()> {
    ...
    *old_location.entry.as_dir()?.mountpoint.lock() = None;
    old_location.mountpoint.children.lock().remove(&old_location.entry.key());

    *new_location.entry.as_dir()?.mountpoint.lock() = Some(self.clone());
    new_location.mountpoint.children.lock().insert(
        new_location.entry.key(),
        Arc::downgrade(self),
    );

    *self.location.lock() = Some(new_location.clone());
    Ok(())
}
```

### 讲解

这里没有创建新的 mountpoint，而是：

1. 从旧父节点摘掉 child mount
2. 挂到新父节点下
3. 更新 mountpoint 自己记录的 `location`

这正是 Linux `MS_MOVE` 的语义：移动现有 mount，而不是复制。

另外本次还补了一个 Linux 风格错误条件：

- 如果 target 是 source 后代，返回 `ELOOP`

这个检查的本质是：沿着 target 的 parent 链向上走，若遇到当前被移动的 mount root，则说明会形成循环。

## 5.7 `MS_RDONLY` / `MS_REMOUNT|MS_RDONLY`：按 mountpoint 拒绝写入

### 在 `sys_mount()` 中设置只读位

```rust
if (flags & MS_REMOUNT) != 0 {
    ...
    if (flags & MS_RDONLY) != 0 {
        target.mountpoint().set_readonly(true);
    }
    return Ok(0);
}
...
let mp = target.bind_mount(&source, (flags & MS_REC) != 0)?;
if (flags & MS_RDONLY) != 0 {
    mp.set_readonly(true);
}
```

### 在 open / write 路径上拒绝写入

```rust
if loc.is_readonly()
    && (flags.intersects(FileFlags::WRITE | FileFlags::APPEND) || self.truncate)
{
    return Err(VfsError::ReadOnlyFilesystem);
}
```

以及：

```rust
if self.inner.location().is_readonly()
    && flags.intersects(FileFlags::WRITE | FileFlags::APPEND)
{
    return Err(VfsError::ReadOnlyFilesystem);
}
```

### 讲解

这里有两个检查点：

1. 打开文件时就拒绝“写/append/truncate”
   - 覆盖新打开的写路径
2. 已经打开的 fd 在后续 `write()` / `append()` 时也检查 mount 是否只读
   - 覆盖“先打开，再 remount 成只读”的情况

这样就能同时满足：

- `MS_RDONLY` 挂载后写入 `EROFS`
- `MS_REMOUNT|MS_RDONLY` 后已有内容保留，但新增写入 `EROFS`
- `MS_REMOUNT|MS_BIND|MS_RDONLY` 只让 bind 视图只读，而源 mount 保持可写

此外，本次还把只读拦截扩展到了元数据/目录修改路径：

- `Location::update_metadata()` 上拒绝 `chmod`
- 目录创建/删除/重命名路径拒绝 `mkdir` / `unlink` / `rename`

这样用户态就能观察到更完整的 Linux 风格 `EROFS` 行为，而不只是普通文件写入失败。

## 5.8 `MNT_EXPIRE`：两阶段过期标记

实现片段：

```rust
if (flags & MNT_EXPIRE) != 0 {
    if !target.mountpoint().mark_expired() {
        return Err(AxError::from(LinuxError::EAGAIN));
    }
}
```

其中：

```rust
pub fn mark_expired(&self) -> bool {
    self.expired.swap(true, Ordering::AcqRel)
}
```

### 讲解

- `mark_expired()` 返回旧值
- 第一次调用时旧值是 `false`
  - `!false == true`，返回 `EAGAIN`
  - 同时把 `expired` 设成 `true`
- 第二次调用时旧值是 `true`
  - 不再返回 `EAGAIN`
  - 流程继续往下走，执行真正的卸载

这正好实现 Linux 的“两阶段 expire 语义”。

## 5.9 `UMOUNT_NOFOLLOW`：no-follow path resolution

实现片段：

```rust
let target = if (flags & UMOUNT_NOFOLLOW) != 0 {
    FS_CONTEXT.lock().resolve_no_follow(target)?
} else {
    FS_CONTEXT.lock().resolve(target)?
};
```

### 讲解

- 普通 `resolve()` 会跟随 symlink
- `resolve_no_follow()` 则把 symlink 当成最终对象本身

如果 `target` 是一个 symlink：

- 普通 `umount2()` 会解析到真实 mount
- 带 `UMOUNT_NOFOLLOW` 时会停在 symlink 这个 inode 上
- 它不是 mount root，于是返回 `EINVAL`
- 真实 mount 保持不变

## 5.10 `MNT_DETACH`：lazy detach

实现片段：

```rust
if (flags & MNT_DETACH) != 0 {
    target.detach_mount()?;
    return Ok(0);
}
```

底层：

```rust
pub fn detach(self: &Arc<Self>) -> VfsResult<()> {
    ...
    location.mountpoint.children.lock().remove(&location.entry.key());
    *location.entry.as_dir()?.mountpoint.lock() = None;
    Ok(())
}
```

### 讲解

这里的关键是：

- `MNT_DETACH` 必须在 busy 检查之前处理
- lazy detach 不等于普通卸载

本次实现做的事情是：

1. 从父 mount tree 的 `children` 中移除当前 mount
2. 清掉父目录项上的 mountpoint 可见入口

结果就是：

- 旧 fd 仍然持有原对象，所以还能读
- 新路径查找看不到这个 mount 了

这正是本次测试验证的语义。

## 6. 各参数组合的结果清单

下面列出本次文档范围内最常见、也是测试里覆盖到的组合。

### 6.1 `mount(2)` 组合清单

| 参数组合 | Linux 结果 | 本次 StarryOS 结果 |
|---|---|---|
| `MS_SHARED | MS_PRIVATE` | `EINVAL` | 已支持 |
| `MS_SHARED | MS_BIND` | `EINVAL` | 已支持 |
| `MS_PRIVATE` | 修改已有 mount 的传播属性 | 已支持当前测试覆盖的 private 语义，停止后续传播 |
| `MS_SHARED` | 修改已有 mount 的传播属性 | 已支持当前测试覆盖的 shared peer 双向传播语义 |
| `MS_SLAVE` | 修改已有 mount 的传播属性 | 已支持当前测试覆盖的 master → slave 单向传播语义 |
| `MS_UNBINDABLE` | 修改已有 mount 的传播属性 | 已支持 bind 禁止与 recursive bind prune |
| propagation flag + `MS_REC` | 递归修改 mount subtree | 已支持 |
| `MS_BIND` | 普通 bind，不带 nested submount | 已支持 |
| `MS_BIND` on subdirectory | bind mount 内部子目录 | 已支持 |
| `MS_BIND|MS_REC` | recursive bind，带 nested submount | 已支持 |
| `MS_BIND|MS_REC` with unbindable child | 剪掉 unbindable child mount | 已支持 |
| `MS_MOVE` | 移动已有 mount | 已支持 |
| `MS_MOVE` to descendant target | `ELOOP` | 已支持 |
| `MS_REMOUNT` | remount 现有 mount，内容保留 | 已支持 |
| `MS_RDONLY` | 挂成只读，写入 `EROFS` | 已支持 |
| `MS_REMOUNT|MS_RDONLY` | remount 成只读，写入 `EROFS` | 已支持 |
| `MS_REMOUNT|MS_BIND|MS_RDONLY` | bind mount 视图只读，source 仍可写 | 已支持 |

### 6.2 `umount2(2)` 组合清单

| 参数组合 | Linux 结果 | 本次 StarryOS 结果 |
|---|---|---|
| 未知 flags | `EINVAL` | 已支持 |
| `MNT_EXPIRE|MNT_DETACH` | `EINVAL` | 已支持 |
| 非 mount point | `EINVAL` | 已支持 |
| busy mount + 普通卸载 | `EBUSY` | 已支持 |
| `MNT_EXPIRE` 第一次 | `EAGAIN` | 已支持 |
| `MNT_EXPIRE` 第二次 | 真正卸载 | 已支持 |
| `UMOUNT_NOFOLLOW` + symlink | `EINVAL` 且真实 mount 不变 | 已支持 |
| `MNT_DETACH` + busy mount | 成功，路径隐藏，旧 fd 继续可用 | 已支持 |

## 7. 测试与实现的对应关系

本次用户态验证集中在：

- `test-suit/starryos/qemu-smp1/system/util-linux/c/src/main.c`

它覆盖的核心点是：

- propagation flags 的非法组合和无副作用保证
- shared/private/slave/unbindable 的真实传播或隔离效果
- propagation-only 调用不应替换现有 mount 内容
- bind / recursive bind / move / remount / readonly
- bind 子目录
- recursive bind 对 unbindable child 的 prune
- move descendant target 的 `ELOOP`
- readonly mount 上的 `chmod` / `rename` / `unlink` / `mkdir`
- `umount2` 的 invalid flags / invalid combo / expire / nofollow / detach

这也是为什么这次修复能够比较扎实：

- 不是只补 syscall 返回码
- 而是同时验证了 mount tree 可见性、副作用、写入行为、路径解析行为

## 8. 当前仍未覆盖或未完全实现的点

虽然 shared subtree 的核心传播路径已经实现，但仍有一些更深层 Linux
mount/namespace 语义没有完全覆盖：

- 更完整的 shared subtree propagation corner case
- 更复杂 namespace 拓扑下的传播行为
- 真实 peer group 生命周期管理
- 复杂多级 shared+slave 拓扑中的 `propagate_from` 展示与边界行为
- `MS_MOVE` 在 shared subtree 中的全部 Linux 限制和传播规则
- detached mount、过挂载堆栈和传播失败时的完整事务回滚
- 其他普通 mount flags，如 `MS_NODEV`、`MS_NOSUID`、`MS_NOEXEC` 等
- `MNT_FORCE` 的真实强制卸载语义

因此更准确地说：

- 这次已经把本轮补的 `mount/umount2` Linux 兼容测试全部打绿
- 而且 shared/private/slave/unbindable / bind / move / readonly / umount2 核心路径已经具备稳定可验证的 Linux 风格语义
- 但并不意味着 Linux 整体 mount namespace 语义已经“全部实现”

## 9. 参考

- [mount(2) - Linux manual page](https://man7.org/linux/man-pages/man2/mount.2.html)
- [umount(2) / umount2(2) - Linux manual page](https://www.man7.org/linux/man-pages/man2/umount2.2.html)
- [mount_namespaces(7) - Linux manual page](https://man7.org/linux/man-pages/man7/mount_namespaces.7.html)
