# Harness OS 知识图谱功能说明

`tools/starry-syscall-harness` 的 GUI 新增了一个独立的 `Knowledge` 页，用于在处理 StarryOS / ArceOS OS 相关 coding 任务时，自动扫描当前仓库并生成结构化知识图谱。

## 目标

该功能解决两个问题：

1. 新接手任务时，需要快速知道当前代码改动落在 OS 哪个子系统。
2. 写代码或做性能分析时，需要把仓库实践和 OS 课本知识关联起来，便于讲解、复盘和教学报告。

## 启动

```bash
python3 tools/starry-syscall-harness/harness.py ui --host 127.0.0.1 --port 8765
```

浏览器打开：

```text
http://127.0.0.1:8765/
```

进入左侧 `Knowledge` tab。

## 页面能力

`Knowledge` 页面包含：

* 当前开发任务输入框。
* 粗粒度 / 细粒度讲解切换。
* OS 子系统知识图谱 SVG。
* 当前任务焦点节点高亮。
* 节点详情面板。

点击图谱节点后，右侧会显示：

* 子系统职责。
* 相关目录。
* 扫描到的代表文件。
* 扫描到的 Rust 符号。
* OS 课本知识。
* 当前仓库实践关联。

## API

GUI 使用以下本地 API：

```text
GET /api/knowledge-graph?task=<当前任务>&granularity=coarse|fine&refresh=0|1
```

也可以让当前 GUI 扫描相邻的本地教学仓库：

```text
GET /api/knowledge-graph?repo_root=../tg-arceos-tutorial&task=分析教程实验&granularity=fine&refresh=1
```

`repo_root` 为空时扫描当前仓库；非空时必须位于当前仓库或当前仓库的父目录下，且只用于静态扫描，不会通过 artifact API 暴露外部文件。

返回结构包含：

* `graph.nodes`：OS 子系统节点。
* `graph.edges`：子系统依赖边。
* `task.focus_node_ids`：当前任务命中的焦点节点。
* `focus.code_explanation`：代码讲解。
* `focus.os_explanation`：OS 课本知识与实践关联。
* `focus.coding_guidance`：编码建议。

## 扫描方式

当前实现是轻量本地静态扫描，不依赖外部服务：

* 扫描 `os/`、`components/`、`drivers/`、`scripts/`、`tools/`、`test-suit/`、`docs/`。
* 对教学仓库额外扫描 `app-*`、`exercise-*`、根 `README.md`、`report.md`、`Cargo.toml`。
* 跳过 `.git`、`target`、`node_modules`、`__pycache__` 等目录。
* 提取 `.rs`、`.toml`、`.py`、`.md`、`.c`、`.h`、`.json` 文件。
* 根据预定义 OS 子系统目录、关键词、当前任务文本、未提交文件或最近提交文件进行匹配。

当前预置的主要节点包括：

* Linux syscall compatibility
* Task / process lifecycle
* Scheduler / wait / synchronization
* Virtual memory / address space
* Allocator / object lifetime
* VFS / file I/O
* rsext4 / block cache
* Block layer / request queue
* virtio-blk driver
* Network stack / socket path
* virtio-net driver
* virtio-vsock / vhost-vsock
* PCI / interrupt / transport
* procfs / debug observability
* qperf / harness / GUI
* Build / rootfs / qemu tests

## 粗/细粒度

`coarse`：

* 面向任务入门和 PPT 总结。
* 每个焦点节点只给职责、关键目录和 OS 概念关联。

`fine`：

* 面向写代码和 code review。
* 展示代表文件、符号、命中文件、实践风险和编码建议。

## 局限

* 当前是启发式静态扫描，不做完整 Rust 语义解析，也不构建精确调用图。
* 任务焦点依赖目录、关键词、git diff 和最近提交文件，不能替代人工读代码。
* 图谱节点是 OS 教学/工程视角下的子系统归类，不等价于 crate 依赖图。
* 该功能不调用 LLM，因此讲解模板是确定性的；后续可接入更细的符号索引或 rustdoc JSON。

## 后续扩展建议

1. 读取 `Cargo.toml` workspace 和 crate dependency，补充 crate 依赖边。
2. 用 `rustdoc --output-format json` 或 `cargo metadata` 生成更精确的符号图。
3. 将 qperf report 的 hotspot category 自动映射到知识图谱节点。
4. 在 GUI 的 qperf report 页点击 hotspot 时，跳转到对应知识图谱节点。
5. 为每个节点补充课程章节、推荐阅读和典型 bug 模式。
