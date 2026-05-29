# tg-arceos-tutorial OS 知识图谱分析报告

## 1. 分析目标

本报告使用当前 harness GUI 的 OS 知识图谱框架，分析外部仓库 `cg24-THU/tg-arceos-tutorial` 的开发任务结构。重点不是重新实现该仓库任务，而是验证知识图谱框架是否能读取教程仓库的源码与文档，并把开发任务映射到 OS 子系统和课程知识点。

## 2. 输入仓库

| 项目 | 值 |
| --- | --- |
| 仓库 | `git@github.com:cg24-THU/tg-arceos-tutorial.git` |
| 本地路径 | `/home/cg24/tg-arceos-tutorial` |
| commit | `e8ae59bf640a6bce005c4dd4e3a99647bd83baec` |
| 工作区状态 | clean |
| 主要文档 | `README.md`、各 `app-*` / `exercise-*` 的 `README.md`、`report.md` |

克隆命令：

```bash
git clone git@github.com:cg24-THU/tg-arceos-tutorial.git /home/cg24/tg-arceos-tutorial
```

知识图谱扫描产物：

```text
target/knowledge-graph/tg-arceos-tutorial.json
```

## 3. 对框架做的最小增强

原始 OS 知识图谱扫描器主要面向 tgoskits 主仓库，默认扫描 `os/`、`components/`、`drivers/`、`scripts/`、`tools/`、`docs/` 等目录。第一次直接扫描 `tg-arceos-tutorial` 时，`files_seen=0`，原因是教程仓库的核心内容位于 `app-*` 和 `exercise-*` 子 crate。

因此本轮做了最小增强：

* `knowledge_graph.py` 支持扫描：
  * 根 `README.md`
  * 根 `report.md`
  * 根 `Cargo.toml`
  * `app-*`
  * `exercise-*`
* 节点路径规则支持 glob，例如 `app-guest*/**`。
* 新增教程相关节点：
  * `unikernel_runtime`
  * `monolithic_user`
  * `hypervisor_guest`
  * `tutorial_packaging`
* GUI 的 `Knowledge` 页新增 `扫描仓库路径` 输入框。
* 后端 API 新增 `repo_root` 查询参数，可从当前 tgoskits GUI 扫描相邻本地仓库。

API smoke test：

```bash
curl --noproxy '*' -s \
  'http://127.0.0.1:8765/api/knowledge-graph?repo_root=/home/cg24/tg-arceos-tutorial&task=tg-arceos-tutorial%20exercise%20docs&granularity=fine&refresh=1'
```

返回：

```text
HTTP 200
repo_root=/home/cg24/tg-arceos-tutorial
nodes=20
edges=27
files_seen=396
```

## 4. 文档内容摘要

根 `README.md` 明确说明该仓库是一个集合 crate，用于把 ArceOS 相关的 `app-*` 和 `exercise-*` 教学 crate 打包进 `bundle/apps.tar.gz`，便于 `cargo clone` 后离线解包。

根文档把内容分成三条主线：

| 主线 | 子目录 | 文档描述 |
| --- | --- | --- |
| unikernel 教学示例 | `app-helloworld`、`app-collections`、`app-readpflash`、`app-childtask`、`app-msgqueue`、`app-fairsched`、`app-readblk`、`app-loadapp` | 从最小启动、标准库集合、MMIO、任务、调度、块设备到文件加载 |
| monolithic kernel 教学示例 | `app-userprivilege`、`app-lazymapping`、`app-runlinuxapp` | 用户态执行、lazy mapping、Linux ELF/syscall |
| hypervisor 教学示例 | `app-guestmode`、`app-guestaspace`、`app-guestvdev`、`app-guestmonolithickernel` | guest mode、guest address space、virtual device、guest monolithic kernel |

根 `README.md` 还列出 5 个 `exercise-*`：

| exercise | 文档任务 |
| --- | --- |
| `exercise-printcolor` | 彩色终端输出，理解 `println!` / console 输出层次 |
| `exercise-hashmap` | 在 `axstd` 中支持 `collections::HashMap` |
| `exercise-altalloc` | 实现 bump-style memory allocator |
| `exercise-ramfs-rename` | 在 ramfs 根文件系统上支持 `rename` |
| `exercise-sysmap` | 实现 `mmap(2)`，使文件映射到用户地址空间 |

根 `report.md` 是已有实验报告，说明这 5 个 exercise 已按“最小必要修改 + 可验证交付”完成，并记录了单题验证和批量回归结果。报告中特别指出：

* `exercise-sysmap` 是最复杂任务，实际调试中从 `mmap` 扩展到 `brk`、`mprotect`、基础 Linux ABI syscall 和交叉编译器 fallback。
* `exercise-ramfs-rename` 涉及 `axfs::RootDirectory` 转发和 `axfs_ramfs::DirNode` 的 rename 实现。
* 批量回归命令覆盖 5 个 exercise，结果为成功 5 个、失败 0 个。

本轮没有重新运行这些 exercise 的 Docker/QEMU 测试，只读取并分析仓库内已有文档和源码。

## 5. 知识图谱扫描结果

扫描命令：

```bash
python3 - <<'PY'
from pathlib import Path
import json, sys
sys.path.insert(0, '/home/cg24/tgoskits/tools/starry-syscall-harness')
from knowledge_graph import build_knowledge_graph
kg = build_knowledge_graph(
    Path('/home/cg24/tg-arceos-tutorial'),
    task='分析 tg-arceos-tutorial app exercise 教学开发任务 文档 内容',
    granularity='fine',
)
Path('target/knowledge-graph').mkdir(parents=True, exist_ok=True)
Path('target/knowledge-graph/tg-arceos-tutorial.json').write_text(
    json.dumps(kg, ensure_ascii=False, indent=2),
    encoding='utf-8',
)
PY
```

整体结果：

| 指标 | 值 |
| --- | ---: |
| scanned files | 396 |
| graph nodes | 20 |
| graph edges | 27 |
| focus nodes | `unikernel_runtime`、`tutorial_packaging`、`monolithic_user`、`allocator`、`vfs_io` |

Top 节点：

| 节点 | 文件 | 行数 | 关键词命中 | 任务分 |
| --- | ---: | ---: | ---: | ---: |
| `unikernel_runtime` ArceOS unikernel apps | 157 | 15054 | 1149 | 29 |
| `tutorial_packaging` Tutorial bundle / scripts | 4 | 553 | 176 | 24 |
| `monolithic_user` Monolithic kernel / user apps | 63 | 7247 | 903 | 22 |
| `allocator` Allocator / object lifetime | 53 | 4967 | 1300 | 16 |
| `vfs_io` VFS / file I/O | 73 | 11334 | 2077 | 14 |
| `hypervisor_guest` Hypervisor / guest execution | 127 | 17779 | 2361 | 12 |
| `task_process` Task / process lifecycle | 54 | 5669 | 561 | 12 |
| `block_layer` Block layer / request queue | 37 | 4301 | 136 | 12 |
| `build_test` Build / rootfs / qemu tests | 105 | 16299 | 1074 | 7 |
| `interrupt_pci` PCI / interrupt / transport | 161 | 21081 | 910 | 6 |

## 6. 开发任务到 OS 知识点的映射

### 6.1 Unikernel Runtime

图谱焦点：`unikernel_runtime`

覆盖目录：

* `app-helloworld`
* `app-collections`
* `app-readpflash`
* `app-childtask`
* `app-msgqueue`
* `app-fairsched`
* `app-readblk`
* `app-loadapp`
* `exercise-printcolor`
* `exercise-hashmap`
* `exercise-altalloc`

OS 讲解：

* 课本知识：unikernel 把应用与内核库静态组合成专用镜像，弱化传统“内核/用户进程”边界，强调按需链接 OS 组件。
* 仓库实践：这些 `app-*` 从 Hello World 开始，逐步引入 `axstd`、任务、调度、MMIO、块设备和文件加载。

对应开发任务：

* `exercise-printcolor` 是最小 console 输出任务。
* `exercise-hashmap` 是 `axstd` 标准库兼容任务。
* `exercise-altalloc` 是内核分配器任务。

### 6.2 Tutorial Packaging

图谱焦点：`tutorial_packaging`

覆盖目录：

* `README.md`
* `Cargo.toml`
* `report.md`
* `scripts/`
* `bundle/`
* `src/`

OS/工程讲解：

* 课本知识关联较弱，主要是课程工程化：如何把多个可运行 OS 实验组织成可分发、可离线恢复、可批量验证的形式。
* 仓库实践：根 README 说明 `cargo clone` 后通过 `scripts/extract_crates.sh` 解包，维护者通过 `scripts/compress_crates.sh` 重新生成 `bundle/apps.tar.gz`。

对应开发任务：

* 维护 bundle 完整性。
* 批量执行 `app-*` / `exercise-*`。
* 保持根 README、子 README、`report.md` 与实际代码一致。

### 6.3 Monolithic User

图谱焦点：`monolithic_user`

覆盖目录：

* `app-userprivilege`
* `app-lazymapping`
* `app-runlinuxapp`
* `exercise-sysmap`

OS 讲解：

* 课本知识：用户/内核态隔离、系统调用 ABI、缺页异常、进程地址空间、ELF 加载、mmap。
* 仓库实践：`app-userprivilege` 展示特权级切换，`app-lazymapping` 展示 demand paging，`app-runlinuxapp` 加载真实 Linux ELF，`exercise-sysmap` 实现 `mmap(2)`。

对应开发任务：

* `exercise-sysmap` 是该仓库最典型的 monolithic/user 任务。
* 它不是单点 `mmap`，实际需要考虑：
  * 文件 fd 到映射内容的读取。
  * 用户地址空间中的页映射。
  * `brk` 堆映射。
  * `mprotect` 和运行时初始化 syscall。
  * musl/gnu 工具链差异。

### 6.4 Allocator

图谱焦点：`allocator`

覆盖目录：

* `exercise-altalloc`
* `exercise-hashmap`
* `exercise-altalloc/modules/axalloc`
* `exercise-altalloc/modules/bump_allocator`

OS 讲解：

* 课本知识：内核分配器负责管理有限物理内存，常见主题包括 bump allocator、page allocator、byte allocator、碎片和生命周期。
* 仓库实践：`exercise-altalloc` 要实现 `BaseAllocator`、`ByteAllocator`、`PageAllocator`；`exercise-hashmap` 则从集合类型侧依赖 allocator/collections 支撑。

对应开发任务：

* `exercise-altalloc` 的核心是双端 bump：
  * byte allocation 从低地址向高地址增长。
  * page allocation 从高地址向低地址增长。
  * 两端相遇时报 `NoMemory`。
* `exercise-hashmap` 的核心是让 `axstd::collections::HashMap` 可用，实际采用本地 patch `axstd` 并引入 `hashbrown`。

### 6.5 VFS / File I/O

图谱焦点：`vfs_io`

覆盖目录：

* `app-loadapp`
* `exercise-ramfs-rename`
* `exercise-sysmap`

OS 讲解：

* 课本知识：VFS 把 syscall 与具体文件系统、设备、缓存解耦；路径解析、inode/dentry、挂载点和 rename 语义是文件系统实验的核心。
* 仓库实践：
  * `app-loadapp` 展示 FAT filesystem、VirtIO block device 和文件加载。
  * `exercise-ramfs-rename` 在 ramfs 根文件系统上实现 `std::fs::rename`。
  * `exercise-sysmap` 的文件映射依赖文件读和磁盘镜像。

对应开发任务：

* `exercise-ramfs-rename` 需要从 `std::fs::rename` 沿 VFS 路径追到：
  * `axfs::root::rename`
  * `RootDirectory::rename`
  * `axfs_ramfs::DirNode`
* 该题文档明确只要求支持 rename，不要求跨目录 move。

## 7. 超出 focus 但重要的图谱节点

### Hypervisor / Guest Execution

虽然当前 task 文本更偏 exercise/docs，`hypervisor_guest` 仍有 127 个文件、17779 行、2361 次关键词命中，说明它是教程仓库的一个大体量主题。

覆盖目录：

* `app-guestmode`
* `app-guestaspace`
* `app-guestvdev`
* `app-guestmonolithickernel`

OS 讲解：

* 课本知识：guest/host 隔离、二阶段地址转换、VM entry/exit、虚拟设备、架构虚拟化扩展。
* 仓库实践：这些 app 从最小 guest mode，逐步扩展到 guest address space、virtual device 和 guest monolithic kernel。

### Build / Rootfs / QEMU Tests

`build_test` 识别到 105 个文件，主要来自各 crate 的：

* `xtask/src/main.rs`
* `scripts/test.sh`
* `configs/*.toml`

这说明教程仓库的开发任务不是单纯写 Rust 代码，而是强依赖多架构构建、QEMU 运行和脚本化验证。根 `report.md` 也说明真实验证覆盖的是 `riscv64`，其他架构因缺少 QEMU 被跳过。

## 8. 框架输出的可读讲解样例

以 `exercise-sysmap` 为例，细粒度讲解应这样使用：

* 代码视角：
  * 先看 `exercise-sysmap/README.md` 的 Requirements / Expectation。
  * 再看 `exercise-sysmap/src/syscall.rs`，定位 `SYS_MMAP`、`SYS_BRK`、兼容 syscall。
  * 再看 `exercise-sysmap/xtask/src/main.rs`，确认 payload 编译和工具链 fallback。
* OS 视角：
  * `mmap` 是用户地址空间管理，不只是文件 I/O。
  * 文件映射要把文件内容读入新映射页。
  * `brk` 要真实扩展并映射 heap。
  * 运行 Linux 用户程序时，动态/静态运行时可能依赖额外 syscall，即使题目表面只要求 `mmap`。

以 `exercise-ramfs-rename` 为例：

* 代码视角：
  * `std::fs::rename` -> `axfs` root -> mounted fs root -> `axfs_ramfs::DirNode`。
  * 注意路径父目录解析和生命周期。
* OS 视角：
  * VFS rename 是目录项原子更新问题。
  * 本题只支持同一父目录内 rename，不支持跨目录 move。

## 9. 结论

本次试用说明，扩展后的 OS 知识图谱框架可以分析 `tg-arceos-tutorial` 这类“多教学 crate 聚合仓库”：

* 能扫描外部本地仓库。
* 能读取根 README、子 crate README 和已有实验报告。
* 能把 5 个 exercise 映射到 allocator、VFS、syscall、memory/user、unikernel runtime 等 OS 知识点。
* 能把 `app-*` 主线分成 unikernel、monolithic user、hypervisor 三条课程路线。
* 能在 GUI 里通过 `repo_root=/home/cg24/tg-arceos-tutorial` 返回同一份图谱。

当前结论是 `PARTIAL PASS`：

* PASS：框架能完成静态扫描、任务定位和 OS 课本知识关联。
* PARTIAL：它仍是启发式目录/关键词扫描，不是精确 Rust 调用图或依赖图。

## 10. 后续建议

1. 为教程仓库增加专门的 `Curriculum` 视图，把 `app-*` / `exercise-*` 按难度和 OS 主题排序。
2. 从每个 README 自动提取 Requirements / Expectation / Verification，生成任务卡片。
3. 用 `Cargo.toml` 和 `xtask` 信息补全每个 crate 的构建命令、架构支持和 QEMU 依赖。
4. 将根 `report.md` 中的验证结果解析成结构化数据，展示每个 exercise 的 PASS/FAIL 状态。
5. 后续如果要做自动 code explanation，可把选中的图谱节点与对应 README 段落、源码符号一起传给报告生成器。

