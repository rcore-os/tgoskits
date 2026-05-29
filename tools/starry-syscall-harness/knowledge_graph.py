#!/usr/bin/env python3
from __future__ import annotations

import re
import subprocess
import time
from fnmatch import fnmatchcase
from dataclasses import dataclass
from pathlib import Path
from typing import Any


MAX_SCAN_FILES = 2600
MAX_FILE_BYTES = 256 * 1024
MAX_SYMBOLS_PER_NODE = 24
MAX_HOT_FILES_PER_NODE = 10
SYMBOL_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?"
    r"(?:(?:async|unsafe|const)\s+)?"
    r"(?:(?:extern\s+\"[^\"]+\")\s+)?"
    r"(?P<kind>fn|struct|enum|trait|impl|mod)\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_:<>]*)?"
)


@dataclass(frozen=True)
class KnowledgeNodeSpec:
    id: str
    label: str
    layer: str
    paths: tuple[str, ...]
    keywords: tuple[str, ...]
    summary: str
    textbook: str
    practice: str


NODE_SPECS: tuple[KnowledgeNodeSpec, ...] = (
    KnowledgeNodeSpec(
        "starry_syscall",
        "Linux syscall compatibility",
        "Kernel API",
        (
            "os/StarryOS/kernel/src/syscall",
            "tools/starry-syscall-harness/probes",
            "components/axerrno",
            "app-runlinuxapp/**",
            "exercise-sysmap/**",
        ),
        ("syscall", "sys_read", "sys_write", "errno", "linux", "compat", "probe", "uapi"),
        "Linux 兼容系统调用入口、参数校验、errno 映射和对拍探针。",
        "系统调用是用户态进入内核态的受控门；课程里关注 trap、ABI、参数复制、错误码和副作用一致性。",
        "StarryOS 通过 syscall 分发表和 harness Linux 对拍验证行为，修 syscall 时应优先让返回值、errno、文件状态和进程状态与 Linux 一致。",
    ),
    KnowledgeNodeSpec(
        "task_process",
        "Task / process lifecycle",
        "Execution",
        (
            "components/starry-process",
            "os/StarryOS/kernel/src/task",
            "components/starry-signal",
            "app-childtask/**",
            "app-msgqueue/**",
            "app-userprivilege/**",
            "app-runlinuxapp/**",
        ),
        ("task", "process", "thread", "fork", "clone", "execve", "waitpid", "signal", "exit"),
        "进程、线程、exec/wait/exit、signal 和 StarryOS 任务生命周期。",
        "进程抽象连接地址空间、文件表、信号和调度实体；wait/exec/fork 是操作系统进程管理的核心接口。",
        "相关代码通常同时影响 syscall 语义、调度等待、资源释放和 mm/file 表生命周期，改动后需要跑 syscall harness 和对应 qemu case。",
    ),
    KnowledgeNodeSpec(
        "scheduler_sync",
        "Scheduler / wait / synchronization",
        "Execution",
        (
            "components/axsched",
            "components/axpoll",
            "components/kspin",
            "components/kernel_guard",
            "components/lockdep",
            "app-childtask/**",
            "app-msgqueue/**",
            "app-fairsched/**",
        ),
        ("schedule", "scheduler", "wait", "waker", "mutex", "spin", "preempt", "poll", "lock", "condvar"),
        "调度器、等待队列、poll/waker、锁和临界区。",
        "课程里的调度与同步主题包括运行队列、阻塞/唤醒、临界区、死锁和优先级反转。",
        "性能分析中 scheduler/wait/lock 热点常说明同步粒度或阻塞路径有问题；语义修复中要防止丢唤醒和错误持锁。",
    ),
    KnowledgeNodeSpec(
        "memory_vm",
        "Virtual memory / address space",
        "Memory",
        (
            "components/starry-vm",
            "os/StarryOS/kernel/src/mm",
            "components/axcpu",
            "components/someboot/src/mem",
            "app-lazymapping/**",
            "app-userprivilege/**",
            "app-runlinuxapp/**",
            "exercise-sysmap/**",
        ),
        ("memory", "vm", "vma", "page", "pte", "mmap", "munmap", "copy_from_user", "copy_to_user", "tlb"),
        "虚拟内存、用户地址空间、页表、mmap 和用户态内存复制。",
        "虚拟内存把进程地址空间映射到物理页；页表权限、缺页处理和用户/内核复制是安全边界。",
        "改动时重点确认用户指针校验、页权限、跨页复制和生命周期；性能热点中 memory_set/drop 常对应地址空间释放或映射维护。",
    ),
    KnowledgeNodeSpec(
        "allocator",
        "Allocator / object lifetime",
        "Memory",
        ("modules/axalloc", "components/axklib", "components/linked_list_r4l", "exercise-altalloc/**", "exercise-hashmap/**"),
        ("alloc", "allocator", "heap", "drop", "Arc", "Box", "Vec", "BTreeMap", "slab"),
        "堆分配、集合生命周期、Arc/Box/Vec/BTreeMap 释放路径。",
        "内核分配器负责管理有限物理内存；大量短生命周期对象会造成锁竞争、碎片和 cache miss。",
        "qperf 中 allocator/drop 热点要结合调用栈判断来源，避免只替换分配器而忽略上层数据结构或批处理机会。",
    ),
    KnowledgeNodeSpec(
        "vfs_io",
        "VFS / file I/O",
        "Storage",
        (
            "components/axfs-ng-vfs",
            "components/axfs_crates",
            "components/axio",
            "os/StarryOS/kernel/src/fs",
            "app-loadapp/**",
            "exercise-ramfs-rename/**",
            "exercise-sysmap/**",
        ),
        ("vfs", "file", "inode", "dentry", "read_at", "write_at", "open", "close", "fd", "pipe"),
        "VFS 抽象、文件描述符、read/write/open/close 和通用 I/O 缓冲。",
        "VFS 把系统调用与具体文件系统、设备和 pipe/socket 解耦；课程里关注文件描述符表、inode、路径解析和缓存。",
        "I/O 性能任务要从 syscall 层沿 VFS 向下看，确认热点来自路径解析、缓存 miss、设备请求还是 copy。",
    ),
    KnowledgeNodeSpec(
        "ext4_cache",
        "rsext4 / block cache",
        "Storage",
        ("components/rsext4",),
        ("ext4", "rsext4", "DataBlockCache", "Jbd2Dev", "cache", "readahead", "journal", "extent", "block"),
        "rsext4 文件系统、data block cache、journal 包装设备和 ext4 元数据。",
        "文件系统把文件偏移映射成磁盘块；缓存与预读利用局部性减少设备 I/O 次数。",
        "本轮 virtio-blk 优化正落在这里：在 data block cache miss 时批量读取连续块，减少 4KiB 同步 virtqueue 请求。",
    ),
    KnowledgeNodeSpec(
        "block_layer",
        "Block layer / request queue",
        "Storage",
        (
            "drivers/interface/rdif-block",
            "drivers/blk/rd-block",
            "drivers/ax-driver/src/block",
            "app-readblk/**",
            "app-loadapp/**",
            "exercise-sysmap/**",
        ),
        ("block", "blk", "Block", "IQueue", "CmdQueue", "read_blocks", "submit_request", "request", "direct"),
        "块设备接口、命令队列、同步/异步 read/write 请求封装。",
        "块层把文件系统块请求转换为设备请求；队列深度和合并策略决定能否发挥设备并行性。",
        "性能任务中应关注请求大小、同步等待、direct DMA 条件和 future/poll 路径，避免每 4KiB 都完整 kick/wait 一次。",
    ),
    KnowledgeNodeSpec(
        "virtio_blk",
        "virtio-blk driver",
        "Device",
        (
            "drivers/ax-driver/src/virtio/block.rs",
            "drivers/ax-driver/src/qperf_metrics.rs",
            "app-readblk/**",
            "app-loadapp/**",
            "exercise-sysmap/**",
        ),
        ("virtio_blk", "VirtIOBlk", "virtqueue", "add_notify_wait_pop", "notify", "kick", "DMA", "queue depth"),
        "StarryOS/ArceOS virtio-blk 适配、DMA buffer、virtqueue submit/wait/pop 和 qperf counters。",
        "virtio 使用 virtqueue 在 guest/host 间传递描述符；性能关键是批量提交、减少 notify/kick、利用 queue depth。",
        "qperf 已证明 blk 顺序读瓶颈是同步小 I/O 导致 add_notify_wait_pop/notify 过多；下一步可做 pending read 或真正异步队列化。",
    ),
    KnowledgeNodeSpec(
        "net_stack",
        "Network stack / socket path",
        "Network",
        ("drivers/ax-driver/src/net", "os/arceos/modules/axnet-ng", "os/StarryOS/kernel/src/net"),
        ("net", "socket", "tcp", "udp", "rx", "tx", "packet", "recv", "send", "copy_within"),
        "网络栈、socket syscall、RX/TX 数据路径和 packet buffer。",
        "网络子系统把 socket API、协议栈和网卡驱动连接起来；吞吐常受 copy、buffer ownership 和锁影响。",
        "后续 net 优化应重点验证 RX copy_within、TX staging copy、inflight 管理和 memcpy/memmove 分类是否下降。",
    ),
    KnowledgeNodeSpec(
        "virtio_net",
        "virtio-net driver",
        "Device",
        ("drivers/ax-driver/src/virtio/net.rs", "drivers/ax-driver/src/net", "drivers/interface"),
        ("virtio_net", "VirtIONet", "rx", "tx", "copy_within", "staging", "BTreeMap", "inflight"),
        "virtio-net RX/TX、staging copy、inflight descriptor 管理和 qperf counters。",
        "virtio-net 同样基于 virtqueue；网络路径还要处理 buffer recycle、包边界和协议栈 ownership。",
        "当前 counters 能看 RX/TX bytes、copy bytes 和 BTree/inflight 操作，适合做去 copy 和 BTreeMap 替换的 A/B。",
    ),
    KnowledgeNodeSpec(
        "virtio_vsock",
        "virtio-vsock / vhost-vsock",
        "Device",
        ("drivers/ax-driver/src/vsock", "os/arceos/modules/axnet-ng/src/device/vsock.rs"),
        ("vsock", "vhost-vsock", "cid", "virtio_socket", "stream"),
        "virtio-vsock/vhost-vsock 设备路径和环境依赖。",
        "vsock 提供 guest/host 或 VM 间 socket-like 通信；实验依赖 host 暴露 `/dev/vhost-vsock`。",
        "当前环境若缺 `/dev/vhost-vsock`，只能记录阻塞并给出补测命令，不能伪造 vsock 性能数据。",
    ),
    KnowledgeNodeSpec(
        "interrupt_pci",
        "PCI / interrupt / transport",
        "Device",
        (
            "drivers/ax-driver/src/pci",
            "drivers/intc",
            "components/someboot/src/fdt",
            "drivers/ax-driver/src/virtio",
            "app-readpflash/**",
            "app-readblk/**",
            "app-loadapp/**",
            "app-guest*/**",
        ),
        ("pci", "interrupt", "irq", "plic", "msix", "transport", "probe", "device tree", "fdt"),
        "PCI/设备发现、中断控制器、virtio transport 和 probe 路径。",
        "设备发现和中断把硬件资源映射给驱动；boot profile 中 PCI probe 热点不应污染 workload 数据面结论。",
        "marker/window 的价值之一就是把 PCI probe 等启动成本从 blk/net workload 分析中排除。",
    ),
    KnowledgeNodeSpec(
        "procfs_debug",
        "procfs / debug observability",
        "Observability",
        ("os/StarryOS/kernel/src/pseudofs", "drivers/ax-driver/src/qperf_metrics.rs"),
        ("procfs", "/proc", "qperf_metrics", "debug", "metric", "counter", "observability"),
        "procfs debug 文件、qperf metrics 导出和 reset 机制。",
        "操作系统常通过 procfs/sysfs/debugfs 暴露运行时状态；这些接口应稳定、低侵入、默认关闭重成本逻辑。",
        "`/proc/qperf_metrics` 是本轮把 driver counters 合入 report.json 的最小侵入出口。",
    ),
    KnowledgeNodeSpec(
        "qperf_tooling",
        "qperf / harness / GUI",
        "Tooling",
        ("tools/qperf", "tools/starry-syscall-harness", "scripts/axbuild/src/starry/perf.rs", "docs/qperf"),
        ("qperf", "harness", "flamegraph", "callchain", "marker", "perf-profile", "perf-compare", "knowledge graph"),
        "qperf plugin/analyzer、harness CLI/MCP/UI、cargo starry perf 和报告生成。",
        "性能工具链对应课程里的 measurement methodology：明确实验窗口、采样偏差、归一化指标和 A/B 对照。",
        "本功能也属于这里：GUI 自动扫描仓库，生成 OS 知识图谱，把当前 coding 任务映射到相关子系统并给出讲解。",
    ),
    KnowledgeNodeSpec(
        "unikernel_runtime",
        "ArceOS unikernel apps",
        "Curriculum",
        (
            "README.md",
            "report.md",
            "app-helloworld/**",
            "app-collections/**",
            "app-readpflash/**",
            "app-childtask/**",
            "app-msgqueue/**",
            "app-fairsched/**",
            "app-readblk/**",
            "app-loadapp/**",
            "exercise-printcolor/**",
            "exercise-hashmap/**",
            "exercise-altalloc/**",
        ),
        ("unikernel", "arceos", "axstd", "helloworld", "collections", "childtask", "msgqueue", "fairsched", "exercise"),
        "ArceOS unikernel 教学应用和基础练习，覆盖启动、标准库、任务、调度、设备和集合/分配器。",
        "Unikernel 把应用和内核库静态组合成一个专用系统镜像，教学重点是最小运行时、库化 OS 和按需启用组件。",
        "tg-arceos-tutorial 中 `app-*` 和基础 `exercise-*` 就是从 Hello World 到任务、调度、设备、分配器的渐进式 unikernel 课程。",
    ),
    KnowledgeNodeSpec(
        "monolithic_user",
        "Monolithic kernel / user apps",
        "Curriculum",
        (
            "app-userprivilege/**",
            "app-lazymapping/**",
            "app-runlinuxapp/**",
            "exercise-sysmap/**",
        ),
        ("monolithic", "user", "privilege", "syscall", "elf", "mmap", "page fault", "linux app", "musl"),
        "ArceOS 单体内核式用户态支持：特权级切换、用户地址空间、ELF 加载、Linux syscall 和 mmap。",
        "单体内核课程主题包括用户/内核态隔离、系统调用 ABI、缺页异常、进程地址空间和可执行文件加载。",
        "教程中的 `app-userprivilege`、`app-lazymapping`、`app-runlinuxapp` 和 `exercise-sysmap` 组成用户态支持学习路径。",
    ),
    KnowledgeNodeSpec(
        "hypervisor_guest",
        "Hypervisor / guest execution",
        "Curriculum",
        (
            "app-guestmode/**",
            "app-guestaspace/**",
            "app-guestvdev/**",
            "app-guestmonolithickernel/**",
        ),
        ("hypervisor", "guest", "vm exit", "nested page fault", "npf", "h-extension", "el2", "svm", "vcpu"),
        "ArceOS hypervisor 教程应用，覆盖 guest mode、guest address space、virtual device 和 guest monolithic kernel。",
        "虚拟化课程主题包括 guest/host 隔离、二阶段地址转换、VM entry/exit、虚拟设备和架构扩展。",
        "教程中的 `app-guest*` 系列展示从最小 guest 到地址空间、虚拟设备、完整 guest kernel 的递进。",
    ),
    KnowledgeNodeSpec(
        "tutorial_packaging",
        "Tutorial bundle / scripts",
        "Tooling",
        ("README.md", "Cargo.toml", "report.md", "scripts/**", "bundle/**", "src/**"),
        ("bundle", "cargo clone", "extract", "compress", "batch", "exercise", "app", "tutorial", "report"),
        "教程聚合仓库的打包、解包、批量运行脚本和实验报告。",
        "教学仓库工程化关注可复现分发、离线解包、批量验证和实验报告证据链。",
        "维护该仓库时，根 README、`scripts/extract_crates.sh`、`scripts/compress_crates.sh`、批量脚本和 `report.md` 是首要入口。",
    ),
    KnowledgeNodeSpec(
        "build_test",
        "Build / rootfs / qemu tests",
        "Tooling",
        ("scripts/axbuild", "scripts/test", "test-suit", ".cargo", "*/xtask/**", "*/scripts/test.sh", "*/configs/**"),
        ("cargo starry", "xtask", "rootfs", "qemu", "clippy", "test", "build", "target"),
        "构建、rootfs、QEMU 运行、测试套件和 clippy/fmt 验证入口。",
        "OS 工程实践离不开可复现构建和自动化测试；实验环境差异会直接影响性能结论。",
        "StarryOS/QEMU/qperf 任务优先走 cargo starry 或 harness 入口，代码改动后要用 targeted clippy 和 smoke test 验证。",
    ),
)


EDGES: tuple[tuple[str, str, str], ...] = (
    ("starry_syscall", "task_process", "syscall creates/waits/exits tasks"),
    ("starry_syscall", "memory_vm", "user pointer validation and mmap"),
    ("starry_syscall", "vfs_io", "read/write/open route into VFS"),
    ("task_process", "scheduler_sync", "tasks block, wake and schedule"),
    ("task_process", "memory_vm", "process owns address space"),
    ("vfs_io", "ext4_cache", "filesystem implementation"),
    ("ext4_cache", "block_layer", "cached blocks become block device I/O"),
    ("block_layer", "virtio_blk", "block requests submit to virtio-blk"),
    ("virtio_blk", "interrupt_pci", "virtio transport and PCI probe"),
    ("net_stack", "virtio_net", "net device backend"),
    ("virtio_net", "interrupt_pci", "virtio transport and IRQ"),
    ("net_stack", "virtio_vsock", "socket-like communication path"),
    ("virtio_vsock", "interrupt_pci", "virtio/vhost transport"),
    ("qperf_tooling", "procfs_debug", "metrics are exported through procfs"),
    ("qperf_tooling", "virtio_blk", "qperf counters and categories explain blk bottlenecks"),
    ("qperf_tooling", "virtio_net", "qperf counters and categories explain net bottlenecks"),
    ("qperf_tooling", "build_test", "cargo starry perf and smoke scripts"),
    ("build_test", "starry_syscall", "syscall harness validates Linux compatibility"),
    ("tutorial_packaging", "unikernel_runtime", "bundle contains unikernel apps and exercises"),
    ("tutorial_packaging", "monolithic_user", "bundle contains monolithic user-mode exercises"),
    ("tutorial_packaging", "hypervisor_guest", "bundle contains hypervisor tutorial apps"),
    ("unikernel_runtime", "scheduler_sync", "task and scheduling apps exercise axtask"),
    ("unikernel_runtime", "allocator", "collections and allocator exercises teach memory management"),
    ("monolithic_user", "starry_syscall", "Linux-like user apps depend on syscall ABI"),
    ("monolithic_user", "memory_vm", "mmap and lazy mapping depend on address spaces"),
    ("hypervisor_guest", "memory_vm", "guest address spaces depend on page tables"),
    ("hypervisor_guest", "interrupt_pci", "guest devices and exits depend on traps and devices"),
)


IGNORE_PARTS = {
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    ".mypy_cache",
}

SCAN_SUFFIXES = {".rs", ".toml", ".py", ".md", ".c", ".h", ".json"}


def build_knowledge_graph(repo_root: Path, *, task: str = "", granularity: str = "coarse") -> dict[str, Any]:
    repo_root = repo_root.resolve()
    granularity = granularity if granularity in {"coarse", "fine"} else "coarse"
    files = collect_files(repo_root)
    task_context = current_task_context(repo_root, task)
    nodes = [scan_node(repo_root, spec, files, task_context) for spec in NODE_SPECS]
    nodes_by_id = {node["id"]: node for node in nodes}
    for node in nodes:
        node["degree"] = sum(1 for source, target, _ in EDGES if source == node["id"] or target == node["id"])
    focused = select_focus_nodes(nodes)
    focus_ids = {node["id"] for node in focused}
    for node in nodes:
        node["focus"] = node["id"] in focus_ids

    return {
        "generated_at": time.time(),
        "repo_root": str(repo_root),
        "granularity": granularity,
        "scan": {
            "files_seen": len(files),
            "max_files": MAX_SCAN_FILES,
            "suffixes": sorted(SCAN_SUFFIXES),
        },
        "task": {
            "query": task,
            "context_terms": task_context["terms"],
            "changed_files": task_context["changed_files"],
            "last_commit": task_context["last_commit"],
            "focus_node_ids": [node["id"] for node in focused],
        },
        "graph": {
            "nodes": nodes,
            "edges": [
                {
                    "source": source,
                    "target": target,
                    "label": label,
                    "focus": source in focus_ids or target in focus_ids,
                }
                for source, target, label in EDGES
                if source in nodes_by_id and target in nodes_by_id
            ],
        },
        "focus": build_focus_explanation(focused, task_context, granularity),
    }


def collect_files(repo_root: Path) -> list[Path]:
    collected: list[Path] = []
    roots = [repo_root / name for name in ("os", "components", "drivers", "scripts", "tools", "test-suit", "docs")]
    roots.extend(sorted(repo_root.glob("app-*")))
    roots.extend(sorted(repo_root.glob("exercise-*")))
    for file_name in ("README.md", "Cargo.toml", "report.md", "book.toml", "src/SUMMARY.md", "src/lib.rs"):
        path = repo_root / file_name
        if path.is_file() and path.suffix in SCAN_SUFFIXES:
            collected.append(path.relative_to(repo_root))
    for root in roots:
        if not root.exists():
            continue
        for path in root.rglob("*"):
            if len(collected) >= MAX_SCAN_FILES:
                return collected
            if not path.is_file() or path.suffix not in SCAN_SUFFIXES:
                continue
            rel = path.relative_to(repo_root)
            if any(part in IGNORE_PARTS for part in rel.parts):
                continue
            collected.append(rel)
    return collected


def scan_node(
    repo_root: Path,
    spec: KnowledgeNodeSpec,
    files: list[Path],
    task_context: dict[str, Any],
) -> dict[str, Any]:
    matched = [path for path in files if path_matches_spec(path, spec)]
    total_lines = 0
    symbols: list[dict[str, Any]] = []
    hot_files: list[dict[str, Any]] = []
    keyword_hits: dict[str, int] = {keyword: 0 for keyword in spec.keywords}

    for rel in matched:
        absolute = repo_root / rel
        try:
            size = absolute.stat().st_size
        except OSError:
            continue
        if size > MAX_FILE_BYTES:
            hot_files.append({"path": rel.as_posix(), "lines": None, "symbols": 0, "note": "skipped large file"})
            continue
        try:
            text = absolute.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        lines = text.splitlines()
        total_lines += len(lines)
        file_symbols = extract_symbols(rel, lines)
        for keyword in spec.keywords:
            keyword_hits[keyword] += count_keyword(text, keyword)
        if file_symbols and len(symbols) < MAX_SYMBOLS_PER_NODE:
            symbols.extend(file_symbols[: max(0, MAX_SYMBOLS_PER_NODE - len(symbols))])
        if len(hot_files) < MAX_HOT_FILES_PER_NODE:
            hot_files.append({"path": rel.as_posix(), "lines": len(lines), "symbols": len(file_symbols)})

    score = score_node(spec, matched, task_context, keyword_hits)
    return {
        "id": spec.id,
        "label": spec.label,
        "layer": spec.layer,
        "summary": spec.summary,
        "textbook": spec.textbook,
        "practice": spec.practice,
        "paths": list(spec.paths),
        "keywords": list(spec.keywords),
        "stats": {
            "files": len(matched),
            "lines": total_lines,
            "symbols": len(symbols),
            "keyword_hits": sum(keyword_hits.values()),
        },
        "hot_files": hot_files,
        "symbols": symbols[:MAX_SYMBOLS_PER_NODE],
        "task_score": score,
        "matched_changed_files": [
            path for path in task_context["changed_files"] if path_matches_spec(Path(path), spec)
        ],
    }


def path_matches_spec(path: Path, spec: KnowledgeNodeSpec) -> bool:
    rel = path.as_posix()
    for base in spec.paths:
        pattern = base.rstrip("/")
        if any(char in pattern for char in "*?[]"):
            if fnmatchcase(rel, pattern) or fnmatchcase(rel, pattern.rstrip("*").rstrip("/") + "/*"):
                return True
            continue
        if rel == pattern or rel.startswith(pattern + "/"):
            return True
    return False


def extract_symbols(path: Path, lines: list[str]) -> list[dict[str, Any]]:
    symbols: list[dict[str, Any]] = []
    for line_no, line in enumerate(lines, start=1):
        match = SYMBOL_RE.match(line)
        if not match:
            continue
        name = match.group("name") or "<anonymous>"
        if name == "<anonymous>" and match.group("kind") != "impl":
            continue
        symbols.append(
            {
                "kind": match.group("kind"),
                "name": name,
                "path": path.as_posix(),
                "line": line_no,
            }
        )
        if len(symbols) >= 8:
            break
    return symbols


def count_keyword(text: str, keyword: str) -> int:
    if not keyword:
        return 0
    return text.lower().count(keyword.lower())


def current_task_context(repo_root: Path, task: str) -> dict[str, Any]:
    changed_files = unique_lines(
        git_output(repo_root, ["diff", "--name-only"]) + git_output(repo_root, ["diff", "--cached", "--name-only"])
    )
    if not changed_files:
        changed_files = unique_lines(git_output(repo_root, ["show", "--name-only", "--format=", "-1"]))
    last_commit = " ".join(git_output(repo_root, ["log", "-1", "--pretty=%h %s"])[:1])
    terms = tokenize(" ".join([task, last_commit, " ".join(changed_files)]))
    return {
        "raw": task,
        "terms": terms[:80],
        "changed_files": changed_files[:80],
        "last_commit": last_commit,
    }


def git_output(repo_root: Path, args: list[str]) -> list[str]:
    try:
        result = subprocess.run(
            ["git", *args],
            cwd=repo_root,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=2,
        )
    except (OSError, subprocess.TimeoutExpired):
        return []
    if result.returncode != 0:
        return []
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def unique_lines(lines: list[str]) -> list[str]:
    seen: set[str] = set()
    unique: list[str] = []
    for line in lines:
        if line not in seen:
            seen.add(line)
            unique.append(line)
    return unique


def tokenize(text: str) -> list[str]:
    tokens = re.findall(r"[A-Za-z0-9_./:-]+|[\u4e00-\u9fff]{2,}", text.lower())
    stop = {"src", "rs", "toml", "md", "the", "and", "for", "with", "this", "that"}
    return [token for token in tokens if len(token) >= 2 and token not in stop]


def score_node(
    spec: KnowledgeNodeSpec,
    matched_files: list[Path],
    task_context: dict[str, Any],
    keyword_hits: dict[str, int],
) -> int:
    score = 0
    terms = task_context["terms"]
    changed_files = [Path(path) for path in task_context["changed_files"]]
    for changed in changed_files:
        if path_matches_spec(changed, spec):
            score += 30
    for term in terms:
        normalized_term = normalize_token(term)
        if any(
            normalized_term in normalize_token(keyword) or normalize_token(keyword) in normalized_term
            for keyword in spec.keywords
        ):
            score += 8
        if normalized_term in normalize_token(spec.id) or normalized_term in normalize_token(spec.label):
            score += 6
        if any(normalized_term in normalize_token(path.as_posix()) for path in matched_files[:80]):
            score += 2
    score += min(sum(keyword_hits.values()) // 200, 10)
    return score


def normalize_token(text: str) -> str:
    return text.lower().replace("-", "_").replace("/", "_").replace("::", "_")


def select_focus_nodes(nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    scored = sorted(nodes, key=lambda item: (item["task_score"], item["stats"]["files"]), reverse=True)
    focus = [node for node in scored if node["task_score"] > 0][:5]
    if focus:
        return focus
    return scored[:3]


def build_focus_explanation(
    focused: list[dict[str, Any]],
    task_context: dict[str, Any],
    granularity: str,
) -> dict[str, Any]:
    if not focused:
        return {
            "summary": "未识别到明确的 OS 子系统焦点。",
            "code_explanation": [],
            "os_explanation": [],
            "coding_guidance": [],
        }

    summary = "当前任务最可能落在：" + "、".join(node["label"] for node in focused[:3])
    code_explanation: list[str] = []
    os_explanation: list[str] = []
    coding_guidance: list[str] = []

    for node in focused:
        changed = node.get("matched_changed_files") or []
        hot_files = [item["path"] for item in node.get("hot_files", [])[:5]]
        symbol_names = [
            f"{item['kind']} {item['name']} ({item['path']}:{item['line']})"
            for item in node.get("symbols", [])[:8]
        ]
        if granularity == "fine":
            code_explanation.append(
                "\n".join(
                    [
                        f"{node['label']}: {node['summary']}",
                        f"相关目录: {', '.join(node['paths'])}",
                        f"当前任务命中文件: {', '.join(changed) if changed else '无未提交命中，参考最近提交或任务关键词'}",
                        f"代表文件: {', '.join(hot_files) if hot_files else '未扫描到文件'}",
                        f"代表符号: {', '.join(symbol_names) if symbol_names else '未提取到 Rust 符号'}",
                    ]
                )
            )
        else:
            code_explanation.append(
                f"{node['label']}: {node['summary']} 主要看 {', '.join(node['paths'][:3])}。"
            )
        os_explanation.append(f"{node['label']}: 课本知识是“{node['textbook']}”；在本仓库里的实践对应是“{node['practice']}”")
        coding_guidance.extend(node_guidance(node))

    if task_context["changed_files"]:
        coding_guidance.insert(0, "当前工作区/最近提交涉及文件：" + ", ".join(task_context["changed_files"][:8]))
    return {
        "summary": summary,
        "code_explanation": code_explanation,
        "os_explanation": os_explanation,
        "coding_guidance": unique_lines(coding_guidance)[:12],
    }


def node_guidance(node: dict[str, Any]) -> list[str]:
    node_id = node["id"]
    if node_id == "virtio_blk":
        return [
            "先看 qperf counters：notify/kick、add_notify_wait_pop、queue depth，再决定是减少请求数还是做异步队列化。",
            "不要把 driver-visible queue depth 直接说成 ring-level 精确统计。",
        ]
    if node_id == "ext4_cache":
        return [
            "文件系统缓存改动需要同时验证顺序读、随机读和 cache 容量边界。",
            "readahead 优化应观察 blk request 数下降是否抵消额外读放大。",
        ]
    if node_id == "starry_syscall":
        return [
            "系统调用语义修复应以 Linux probe 输出为准，避免弱化测试来隐藏差异。",
            "改动后优先跑对应 arch 的 harness discover 或最小 qemu case。",
        ]
    if node_id == "memory_vm":
        return [
            "用户指针、页权限和跨页复制是内存相关 bug 的高风险点。",
            "涉及地址空间生命周期时关注 drop 路径和调度切换时机。",
        ]
    if node_id in {"virtio_net", "net_stack"}:
        return [
            "net 优化先用 qperf counters 验证 RX/TX bytes、copy bytes 和 inflight 操作。",
            "copy 优化需要确认 buffer ownership 和协议栈生命周期没有被破坏。",
        ]
    if node_id == "qperf_tooling":
        return [
            "工具链改动后至少做 py_compile、前端语法检查和一个 API smoke test。",
            "报告里区分真实采样、后处理过滤、逻辑归因和 instrumentation counters。",
        ]
    return ["改动前先沿知识图谱查看上游 API、下游调用者和对应测试入口。"]
