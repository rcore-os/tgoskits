#!/usr/bin/env python3
"""Generate per-crate technical documents under docs/crates/.

The generator is intentionally heuristic-driven: it combines Cargo metadata,
source layout, doc comments, local dependency graphs, and path-based project
classification to produce consistent reference docs for every crate in the repo.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import os
import re
import textwrap
import tomllib
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
DOCS_ROOT = REPO_ROOT / "docs" / "crates"
IGNORE_DIRS = {
    ".git",
    ".idea",
    ".cursor",
    ".vscode",
    "__pycache__",
    "target",
    "node_modules",
}
MAX_LIST = 12

MODULE_HINTS = {
    "api": "对外接口与能力封装",
    "arch": "按 CPU 架构分派底层实现",
    "boot": "早期启动与引导协作逻辑",
    "config": "配置模型、解析与静态参数装配",
    "console": "控制台输出与终端交互",
    "context": "执行上下文保存与切换",
    "cpu": "CPU 本地状态与特权级控制",
    "device": "设备抽象、枚举与访问封装",
    "driver": "驱动注册、匹配或设备编排逻辑",
    "entry": "顶层入口与生命周期编排",
    "error": "错误码、错误类型或异常传播约定",
    "file": "文件对象、FD 表和文件系统入口",
    "fs": "文件系统、挂载或路径解析逻辑",
    "hal": "硬件抽象层与平台接口桥接",
    "hypercall": "Hypercall 分发与宿主/客户机桥接",
    "image": "镜像下载、解析与打包流程",
    "imp": "内部实现细节与 trait/backend 绑定",
    "init": "初始化顺序与全局状态建立",
    "input": "输入子系统和事件转换",
    "interrupt": "中断分发与处理器注册逻辑",
    "io": "I/O 抽象、读写缓冲与设备接口",
    "ipc": "进程/线程间通信与同步交互",
    "irq": "IRQ 注册、屏蔽与派发路径",
    "klib": "内核库适配与工具辅助层",
    "lang_items": "裸机语言项和运行时补齐",
    "loader": "ELF 或镜像装载与地址空间布局",
    "logo": "启动信息和标识输出",
    "main": "主入口与编排逻辑",
    "mem": "物理/虚拟内存描述与地址转换",
    "mm": "地址空间、页表或内存管理主逻辑",
    "mp": "多核启动与 CPU 协同初始化",
    "net": "网络栈、socket 或协议适配",
    "path": "路径、名称解析或目录项辅助逻辑",
    "pseudofs": "伪文件系统与虚拟节点挂载",
    "sched": "调度策略与就绪队列管理",
    "shell": "命令解释与交互式控制台",
    "signal": "信号投递、屏蔽和处理路径",
    "socket": "socket 状态机与收发接口",
    "sys": "系统级辅助 API 或底层寄存器抽象",
    "syscall": "系统调用分发与参数编解码",
    "task": "任务/线程/进程生命周期与调度协作",
    "tests": "测试辅助与场景验证代码",
    "time": "时钟、定时器与时间转换逻辑",
    "timer": "定时器队列和超时唤醒路径",
    "utils": "通用工具函数和辅助类型",
    "vcpu": "vCPU 状态机与虚拟 CPU 调度逻辑",
    "vmm": "虚拟机管理器编排与 VM 生命周期控制",
    "vm": "VM 实体、资源模型与状态管理",
    "wire": "协议格式编解码与线协议表示",
}

KEYWORD_MECHANISMS = {
    "alloc": "内存分配器初始化、扩容或对象分配路径",
    "allocator": "内存分配器初始化、扩容或对象分配路径",
    "bitmap": "位图索引、空闲位搜索与资源分配",
    "buddy": "伙伴系统内存分配与合并回收",
    "cfs": "CFS 风格公平调度或时间片分配",
    "clone": "进程/线程复制与共享资源建模",
    "config": "静态配置建模、编译期注入或 TOML 解析",
    "congestion": "拥塞控制、窗口调整与重传策略",
    "dma": "DMA 缓冲分配与地址映射",
    "elf": "ELF 解析、段装载与入口点设置",
    "ept": "二级页表或 EPT/NPT 映射维护",
    "exec": "可执行映像切换与上下文重装载",
    "extent": "extent/区间树管理与块映射",
    "futex": "futex 等待/唤醒与并发同步",
    "gic": "中断控制器状态编排与虚拟中断注入",
    "hash": "哈希索引、查找和冲突组织",
    "hypercall": "hypercall 分发与宿主服务桥接",
    "init": "初始化顺序控制与全局状态建立",
    "interrupt": "中断注册、派发和屏蔽控制",
    "ipi": "跨核 IPI 协调与唤醒路径",
    "irq": "中断注册、派发和屏蔽控制",
    "loader": "镜像/程序装载与地址空间布置",
    "page": "页级映射、页表维护与地址空间布局",
    "paging": "页级映射、页表维护与地址空间布局",
    "plic": "平台中断控制器路由与优先级管理",
    "poll": "事件轮询与 I/O 多路复用",
    "process": "进程生命周期、资源共享与回收",
    "queue": "队列管理、调度或异步事件缓存",
    "ramfs": "内存文件系统对象管理与目录树维护",
    "reassembly": "报文重组与乱序缓存处理",
    "reno": "TCP Reno 拥塞控制策略",
    "rr": "时间片轮转调度策略",
    "schedule": "调度策略与就绪队列维护",
    "sched": "调度策略与就绪队列维护",
    "signal": "信号投递、屏蔽和唤醒协作",
    "slab": "slab/对象缓存分配策略",
    "sleep": "睡眠、超时与阻塞唤醒",
    "smoltcp": "轻量网络栈集成与协议处理",
    "socket": "socket 状态机与连接管理",
    "syscall": "系统调用编解码、分发和错误映射",
    "timer": "定时器触发、截止时间维护和延迟队列",
    "tree": "树形索引、层级组织或遍历加速结构",
    "vcpu": "vCPU 状态机、VM exit 处理与宿主调度桥接",
    "vgic": "虚拟 GIC 模型与中断注入机制",
    "virtio": "VirtIO 设备队列、协商与数据通路",
    "vm": "虚拟机生命周期、资源模型与状态切换",
    "vsock": "虚拟 socket 通道管理",
    "wait": "等待队列、阻塞/唤醒协作",
}

ROLE_HINTS = {
    "components/": "可复用基础组件",
    "components/axdriver_crates/": "驱动子工作区组件",
    "components/axfs_crates/": "文件系统子工作区组件",
    "components/axmm_crates/": "内存管理子工作区组件",
    "components/axplat_crates/": "平台抽象与板级子工作区组件",
    "components/crate_interface/": "跨 crate 接口/宏设施",
    "components/page_table_multiarch/": "多架构页表子工作区组件",
    "components/percpu/": "Per-CPU 子工作区组件",
    "components/ctor_bare/": "裸机构造器与宏支持组件",
    "os/arceos/modules/": "ArceOS 内核模块",
    "os/arceos/api/": "ArceOS 公共 API/feature 聚合层",
    "os/arceos/ulib/": "ArceOS 用户库层",
    "os/arceos/examples/": "ArceOS 示例程序",
    "os/arceos/tools/": "ArceOS 配套工具与辅助程序",
    "os/StarryOS/kernel/": "StarryOS 内核核心",
    "os/StarryOS/starryos/": "StarryOS 启动镜像入口",
    "os/axvisor/": "Axvisor Hypervisor 运行时",
    "platform/": "平台/板级适配层",
    "scripts/": "宿主侧构建与开发工具",
    "xtask/": "根工作区任务编排工具",
    "test-suit/": "系统级测试与回归入口",
}

FEATURE_LIKE_CRATES = {
    "ax-feat",
}

CURATED_DOCS = {
    "ax-hal",
    "aarch64_sysreg",
    "ax-task",
    "axvm",
    "starry-kernel",
    "starry-process",
    "starry-signal",
    "starry-vm",
    "starryos",
    "starryos-test",
    "axvisor",
    "ax-alloc",
    "ax-allocator",
    "axbacktrace",
    "ax-errno",
    "ax-ipi",
    "axklib",
    "ax-libc",
    "ax-log",
    "ax-std",
    "ax-runtime",
    "ax-mm",
    "ax-driver",
    "ax-driver-base",
    "ax-driver-block",
    "ax-driver-display",
    "ax-driver-input",
    "ax-driver-net",
    "ax-driver-pci",
    "ax-driver-virtio",
    "ax-driver-vsock",
    "ax-api",
    "arceos-affinity",
    "ax-helloworld",
    "ax-helloworld-myplat",
    "ax-httpclient",
    "ax-httpserver",
    "arceos-irq",
    "arceos-memtest",
    "arceos-parallel",
    "arceos-priority",
    "ax-shell",
    "arceos-sleep",
    "arceos-wait-queue",
    "arceos-yield",
    "ax-posix-api",
    "axbuild",
    "ax-config",
    "ax-config-gen",
    "ax-config-macros",
    "ax-feat",
    "ax-fs",
    "ax-fs-ng",
    "axfs-ng-vfs",
    "ax-fs-devfs",
    "ax-fs-ramfs",
    "ax-fs-vfs",
    "axhvc",
    "axvmconfig",
    "axaddrspace",
    "axdevice_base",
    "axvcpu",
    "axvisor_api",
    "ax-page-table-multiarch",
    "ax-page-table-entry",
    "ax-memory-addr",
    "ax-memory-set",
    "ax-sync",
    "ax-sched",
    "ax-cpu",
    "ax-io",
    "ax-net",
    "ax-net-ng",
    "axpoll",
    "ax-plat",
    "ax-plat-aarch64-bsta1000b",
    "axdevice",
    "ax-display",
    "ax-dma",
    "ax-input",
    "bwbench-client",
    "cargo-axplat",
    "axplat-dyn",
    "ax-plat-aarch64-qemu-virt",
    "ax-plat-aarch64-peripherals",
    "ax-plat-aarch64-phytium-pi",
    "ax-plat-aarch64-raspi",
    "ax-plat-loongarch64-qemu-virt",
    "ax-plat-riscv64-qemu-virt",
    "ax-plat-x86-pc",
    "axplat-x86-qemu-q35",
    "ax-plat-macros",
    "arm_vcpu",
    "arm_vgic",
    "ax-arm-pl011",
    "ax-arm-pl031",
    "ax-crate-interface",
    "ax-crate-interface-lite",
    "define-simple-traits",
    "define-weak-traits",
    "ax-ctor-bare",
    "ax-ctor-bare-macros",
    "ax-cap-access",
    "bitmap-allocator",
    "ax-cpumask",
    "deptool",
    "ax-handler-table",
    "ax-int-ratio",
    "ax-kernel-guard",
    "ax-kspin",
    "ax-lazyinit",
    "ax-linked-list-r4l",
    "mingo",
    "ax-percpu-macros",
    "impl-simple-traits",
    "impl-weak-partial",
    "impl-weak-traits",
    "hello-kernel",
    "irq-kernel",
    "rsext4",
    "range-alloc-arceos",
    "ax-riscv-plic",
    "scope-local",
    "smoltcp",
    "smoltcp-fuzz",
    "smp-kernel",
    "test-simple",
    "test-weak",
    "test-weak-partial",
    "ax-timer-list",
    "tg-xtask",
    "ax-percpu",
    "x86_vcpu",
    "axvisor_api_proc",
    "riscv-h",
    "riscv_vcpu",
    "riscv_vplic",
}

HW_COMPONENT_PREFIXES = (
    "components/arm_",
    "components/riscv_",
    "components/aarch64_",
    "components/x86_",
    "components/axdriver_crates/",
)


@dataclasses.dataclass
class DepRef:
    key: str
    actual_name: str
    kind: str
    target: str | None
    optional: bool
    local_name: str | None


@dataclasses.dataclass
class Package:
    name: str
    version: str
    description: str
    manifest_path: Path
    dir_path: Path
    rel_dir: str
    manifest: dict
    workspace_root: Path | None
    workspace_rel: str | None
    is_proc_macro: bool
    has_lib: bool
    has_bin: bool
    lib_path: Path | None
    bin_paths: list[Path]
    build_rs: Path | None
    readme_path: Path | None
    root_doc: str
    category: str
    role: str
    source_files: list[Path]
    top_modules: list[tuple[str, list[str]]]
    module_descriptions: list[str]
    public_structs: list[str]
    public_enums: list[str]
    public_traits: list[str]
    public_types: list[str]
    public_statics: list[str]
    public_functions: list[str]
    init_functions: list[str]
    path_keywords: list[str]
    heuristics: list[str]
    dep_refs: list[DepRef]
    direct_local_deps: list[str]
    external_deps: list[str]
    transitive_local_deps: list[str]
    reverse_direct: list[str]
    reverse_transitive: list[str]
    integration_tests: list[str]
    unit_test_files: list[str]
    example_files: list[str]
    bench_files: list[str]
    fuzz_files: list[str]


def read_toml(path: Path) -> dict:
    with path.open("rb") as f:
        return tomllib.load(f)


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return path.read_text(encoding="utf-8", errors="ignore")


def path_matches(rel_dir: str, prefix: str) -> bool:
    clean = prefix.rstrip("/")
    return rel_dir == clean or rel_dir.startswith(prefix)


def compact_doc(text: str, max_len: int = 180) -> str:
    text = re.sub(r"`+", "", text)
    text = re.sub(r"\[[^\]]+\]\([^)]+\)", lambda m: m.group(0).split("]")[0].lstrip("["), text)
    text = re.sub(r"#+\s*", "", text)
    text = re.sub(r"\s+", " ", text).strip()
    if len(text) <= max_len:
        return text
    return text[: max_len - 1].rstrip() + "…"


def strip_comments(text: str) -> str:
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    lines = [line for line in text.splitlines() if not line.strip().startswith("//")]
    return "\n".join(lines)


def resolve_workspace_package_value(
    pkg_info: dict,
    key: str,
    workspace_manifest: dict | None,
    default: str = "",
) -> str:
    value = pkg_info.get(key, default)
    if isinstance(value, dict) and value.get("workspace") is True and workspace_manifest:
        ws_pkg = workspace_manifest.get("workspace", {}).get("package", {})
        inherited = ws_pkg.get(key, default)
        if isinstance(inherited, dict):
            return str(inherited.get("value", default))
        return str(inherited)
    if isinstance(value, dict):
        return str(value.get("value", default))
    return str(value)


def iter_cargo_manifests(root: Path) -> list[Path]:
    manifests: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in IGNORE_DIRS]
        if "Cargo.toml" in filenames:
            manifests.append(Path(dirpath) / "Cargo.toml")
    return sorted(manifests)


def find_workspace_root(pkg_dir: Path) -> Path | None:
    cur = pkg_dir
    while True:
        manifest = cur / "Cargo.toml"
        if manifest.exists():
            data = read_toml(manifest)
            if "workspace" in data:
                return cur
        if cur == REPO_ROOT:
            break
        if cur.parent == cur:
            break
        cur = cur.parent
    return None


def classify_role(rel_dir: str) -> tuple[str, str]:
    for prefix, role in ROLE_HINTS.items():
        if path_matches(rel_dir, prefix):
            if prefix.startswith("components/"):
                return "组件层", role
            if prefix.startswith("os/arceos/"):
                return "ArceOS 层", role
            if prefix.startswith("os/StarryOS/"):
                return "StarryOS 层", role
            if prefix.startswith("os/axvisor/"):
                return "Axvisor 层", role
            if prefix.startswith("platform/"):
                return "平台层", role
            if prefix.startswith("scripts/") or prefix.startswith("xtask/"):
                return "工具层", role
            if prefix.startswith("test-suit/"):
                return "测试层", role
    return "其他", "仓库内普通 crate"


def choose_readme(pkg_dir: Path, manifest: dict) -> Path | None:
    readme = manifest.get("package", {}).get("readme")
    if readme:
        path = (pkg_dir / readme).resolve()
        if path.exists():
            return path
    for name in ("README.md", "README.zh-cn.md", "README.zh_CN.md", "README.txt"):
        path = pkg_dir / name
        if path.exists():
            return path
    return None


def normalize_doc_line(line: str) -> str:
    line = re.sub(r"^\s*//[/!]\s?", "", line)
    line = re.sub(r"^\s*/\*\*?\s?", "", line)
    line = re.sub(r"\s*\*/\s*$", "", line)
    return line.strip()


def extract_leading_doc(path: Path) -> str:
    if not path or not path.exists():
        return ""
    lines = read_text(path).splitlines()
    docs: list[str] = []
    started = False
    for line in lines[:80]:
        stripped = line.strip()
        if stripped.startswith("//!") or stripped.startswith("///"):
            docs.append(normalize_doc_line(stripped))
            started = True
            continue
        if started and stripped == "":
            docs.append("")
            continue
        if not stripped or stripped.startswith("//"):
            if started:
                docs.append("")
            continue
        break
    text = " ".join(x for x in docs if x)
    text = re.sub(r"\s+", " ", text).strip()
    return compact_doc(text)


MOD_DECL_RE = re.compile(r"^\s*(?:pub\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
CFG_FEATURE_RE = re.compile(r'feature\s*=\s*"([^"]+)"')
CFG_TARGET_RE = re.compile(r'target_[a-z_]+\s*=\s*"([^"]+)"')
PUB_STRUCT_RE = re.compile(r"^\s*(?:pub\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)", re.M)
PUB_ENUM_RE = re.compile(r"^\s*(?:pub\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)", re.M)
PUB_TRAIT_RE = re.compile(r"^\s*pub\s+trait\s+([A-Za-z_][A-Za-z0-9_]*)", re.M)
PUB_TYPE_RE = re.compile(r"^\s*(?:pub\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)", re.M)
PUB_STATIC_RE = re.compile(
    r"^\s*(?:pub\s+)?(?:static|const)\s+([A-Za-z_][A-Za-z0-9_]*)", re.M
)
PUB_FN_RE = re.compile(
    r"^\s*pub\s+(?:unsafe\s+)?(?:async\s+)?(?:const\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)
ALL_FN_RE = re.compile(
    r"^\s*(?:pub\s+)?(?:unsafe\s+)?(?:async\s+)?(?:const\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)
PUB_USE_BRACE_RE = re.compile(r"pub\s+use\s+[^{]+{([^}]+)}")
CAMEL_OR_SNAKE_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def extract_top_modules(root_file: Path) -> list[tuple[str, list[str]]]:
    if not root_file or not root_file.exists():
        return []
    mods: list[tuple[str, list[str]]] = []
    pending_attrs: list[str] = []
    for line in read_text(root_file).splitlines():
        stripped = line.strip()
        if stripped.startswith("#["):
            pending_attrs.append(stripped)
            continue
        match = MOD_DECL_RE.match(stripped)
        if match:
            mods.append((match.group(1), pending_attrs.copy()))
            pending_attrs.clear()
            continue
        if stripped and not stripped.startswith("//"):
            pending_attrs.clear()
    return mods


def resolve_module_file(root_file: Path, mod_name: str) -> Path | None:
    parent = root_file.parent
    for candidate in (parent / f"{mod_name}.rs", parent / mod_name / "mod.rs"):
        if candidate.exists():
            return candidate
    return None


def describe_module(root_file: Path, mod_name: str, attrs: list[str]) -> str:
    mod_file = resolve_module_file(root_file, mod_name)
    doc = extract_leading_doc(mod_file) if mod_file else ""
    note = ""
    attr_text = " ".join(attrs)
    features = CFG_FEATURE_RE.findall(attr_text)
    targets = CFG_TARGET_RE.findall(attr_text)
    if features:
        note = f"（按 feature: {', '.join(features)} 条件启用）"
    elif targets:
        note = f"（按目标架构/平台: {', '.join(targets)} 分派）"
    elif "cfg(" in attr_text:
        note = "（按条件编译启用）"
    if doc:
        doc = compact_doc(doc.rstrip("."))
        return f"`{mod_name}`：{doc}{note}"
    hint = MODULE_HINTS.get(mod_name, "内部子模块")
    return f"`{mod_name}`：{hint}{note}"


def collect_source_files(pkg_dir: Path) -> list[Path]:
    files: list[Path] = []
    for sub in ("src", "tests", "examples", "benches", "fuzz"):
        subdir = pkg_dir / sub
        if not subdir.exists():
            continue
        for path in subdir.rglob("*.rs"):
            files.append(path)
    if (pkg_dir / "build.rs").exists():
        files.append(pkg_dir / "build.rs")
    return sorted(files)


def unique(items: list[str]) -> list[str]:
    seen = set()
    out: list[str] = []
    for item in items:
        if item not in seen:
            seen.add(item)
            out.append(item)
    return out


def extract_public_items(files: list[Path]) -> tuple[list[str], list[str], list[str], list[str], list[str], list[str]]:
    structs: list[str] = []
    enums: list[str] = []
    traits: list[str] = []
    types: list[str] = []
    statics: list[str] = []
    functions: list[str] = []
    reexports: list[str] = []
    for path in files:
        text = strip_comments(read_text(path))
        structs.extend(PUB_STRUCT_RE.findall(text))
        enums.extend(PUB_ENUM_RE.findall(text))
        traits.extend(PUB_TRAIT_RE.findall(text))
        types.extend(PUB_TYPE_RE.findall(text))
        statics.extend(PUB_STATIC_RE.findall(text))
        functions.extend(PUB_FN_RE.findall(text))
        for block in PUB_USE_BRACE_RE.findall(text):
            reexports.extend(
                token
                for token in CAMEL_OR_SNAKE_RE.findall(block)
                if token not in {"self", "super", "crate"}
            )
    types = unique(types + reexports)
    return (
        unique(structs),
        unique(enums),
        unique(traits),
        unique(types),
        unique(statics),
        unique(functions),
    )


def extract_init_functions(files: list[Path]) -> list[str]:
    candidates: list[str] = []
    keywords = (
        "init",
        "new",
        "build",
        "create",
        "spawn",
        "start",
        "run",
        "boot",
        "load",
        "mount",
        "setup",
        "register",
        "poll",
        "dispatch",
        "schedule",
        "alloc",
        "map",
        "parse",
        "open",
        "resume",
        "suspend",
    )
    for path in files:
        for func in ALL_FN_RE.findall(strip_comments(read_text(path))):
            if func == "main":
                candidates.append(func)
                continue
            if any(
                func.startswith(key)
                or func.endswith(f"_{key}")
                or f"_{key}_" in func
                for key in keywords
            ):
                candidates.append(func)
    return unique(candidates)


def collect_test_layout(pkg_dir: Path, source_files: list[Path]) -> tuple[list[str], list[str], list[str], list[str], list[str]]:
    integration = []
    unit = []
    examples = []
    benches = []
    fuzz = []
    for path in source_files:
        rel = path.relative_to(pkg_dir).as_posix()
        if rel.startswith("tests/"):
            integration.append(rel)
        elif rel.startswith("examples/"):
            examples.append(rel)
        elif rel.startswith("benches/"):
            benches.append(rel)
        elif rel.startswith("fuzz/"):
            fuzz.append(rel)
        else:
            text = read_text(path)
            if "#[cfg(test)]" in text or "mod tests" in text:
                unit.append(rel)
    return unique(integration), unique(unit), unique(examples), unique(benches), unique(fuzz)


def detect_keywords(pkg: Package) -> list[str]:
    keywords: list[str] = []
    tokens = set()
    tokens.update(part.lower() for part in pkg.name.replace("-", "_").split("_"))
    tokens.update(part.lower() for part in Path(pkg.rel_dir).parts)
    tokens.update(mod.lower() for mod, _ in pkg.top_modules)
    for file in pkg.source_files:
        stem = file.stem.lower()
        if stem not in {"lib", "main", "mod", "build"}:
            tokens.add(stem)
    for token in sorted(tokens):
        for key in KEYWORD_MECHANISMS:
            if len(key) <= 2:
                matched = (
                    token == key
                    or token.startswith(f"{key}_")
                    or token.endswith(f"_{key}")
                    or f"_{key}_" in token
                )
            else:
                matched = (
                    token == key
                    or token.startswith(key)
                    or token.endswith(key)
                    or token.startswith(f"{key}_")
                    or token.endswith(f"_{key}")
                    or f"_{key}_" in token
                )
            if matched:
                keywords.append(key)
                break
    return unique(keywords)


def summarize_mechanisms(pkg: Package) -> list[str]:
    results: list[str] = []
    style = doc_style(pkg)
    if pkg.name in FEATURE_LIKE_CRATES:
        results.append("该 crate 以 Cargo feature 编排和能力选择为主，核心价值在编译期装配而非运行时复杂算法。")
        return results
    if pkg.is_proc_macro:
        results.append("该 crate 的核心机制是过程宏展开、语法树转换或代码生成，重点在编译期接口契约而非运行时数据结构。")
        return results
    if style == "test_suite":
        results.append("该 crate 主要承载系统级测试入口、QEMU/平台配置或断言编排，核心机制是测试场景构造与结果判定。")
        return results
    if pkg.category == "工具层":
        results.append("该 crate 主要实现宿主侧命令编排、配置解析和构建流水线控制，复杂度集中在任务编排而非内核热路径算法。")
    if style in {"platform", "platform_example"}:
        results.append("该 crate 以平台初始化、板级寄存器配置和硬件能力接线为主，算法复杂度次于时序与寄存器语义正确性。")
    if pkg.has_bin and not pkg.has_lib:
        results.append("该 crate 是入口/编排型二进制，复杂度主要来自初始化顺序、配置注入和对下层模块的串接。")
    for key in pkg.path_keywords[:4]:
        results.append(KEYWORD_MECHANISMS[key])
    if not results and pkg.top_modules:
        results.append("该 crate 的实现主要围绕顶层模块分工展开，重点在子系统边界、trait/类型约束以及初始化流程。")
    return unique(results)


def trim_list(items: list[str], limit: int = MAX_LIST) -> tuple[list[str], int]:
    items = unique(items)
    if len(items) <= limit:
        return items, 0
    return items[:limit], len(items) - limit


def format_markdown_list(items: list[str], empty: str) -> str:
    items, more = trim_list(items)
    if not items:
        return f"- {empty}"
    lines = [f"- `{item}`" for item in items]
    if more:
        lines.append(f"- 另外还有 `{more}` 个同类项未在此展开")
    return "\n".join(lines)


def format_bullet_lines(items: list[str], empty: str) -> str:
    items, more = trim_list(items)
    if not items:
        return f"- {empty}"
    lines = [f"- {item}" for item in items]
    if more:
        lines.append(f"- 另外还有 `{more}` 个同类项未在此展开")
    return "\n".join(lines)


def format_inline_list(items: list[str], empty: str, limit: int = MAX_LIST) -> str:
    items, more = trim_list(items, limit)
    if not items:
        return empty
    text = "、".join(f"`{item}`" for item in items)
    if more:
        text += f" 等（另有 {more} 项）"
    return text


def project_of_package(pkg: Package) -> str | None:
    if path_matches(pkg.rel_dir, "os/arceos/") or path_matches(pkg.rel_dir, "test-suit/arceos/"):
        return "arceos"
    if path_matches(pkg.rel_dir, "os/StarryOS/") or path_matches(pkg.rel_dir, "test-suit/starryos/"):
        return "starryos"
    if path_matches(pkg.rel_dir, "os/axvisor/"):
        return "axvisor"
    return None


def dependency_sections(manifest: dict) -> list[tuple[str, str | None, str, object]]:
    sections: list[tuple[str, str | None, str, object]] = []
    for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
        for key, value in manifest.get(kind, {}).items():
            sections.append((kind, None, key, value))
    for target, target_map in manifest.get("target", {}).items():
        for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
            for key, value in target_map.get(kind, {}).items():
                sections.append((kind, target, key, value))
    return sections


def normalize_dep_spec(
    key: str,
    value: object,
    workspace_manifest: dict | None,
) -> tuple[str, bool, str | None]:
    spec = value
    if isinstance(spec, str):
        return key, False, None
    if not isinstance(spec, dict):
        return key, False, None
    if spec.get("workspace") is True and workspace_manifest:
        wsdeps = workspace_manifest.get("workspace", {}).get("dependencies", {})
        inherited = wsdeps.get(key)
        merged: dict = {}
        if isinstance(inherited, str):
            merged["version"] = inherited
        elif isinstance(inherited, dict):
            merged.update(inherited)
        for k, v in spec.items():
            if k != "workspace":
                merged[k] = v
        spec = merged
    actual_name = spec.get("package", key)
    path = spec.get("path")
    optional = bool(spec.get("optional", False))
    return actual_name, optional, path


def resolve_dep_local_name(
    key: str,
    actual_name: str,
    path_str: str | None,
    pkg_dir: Path,
    packages_by_dir: dict[Path, Package],
    packages_by_name: dict[str, Package],
) -> str | None:
    if path_str:
        dep_dir = (pkg_dir / path_str).resolve()
        if dep_dir.is_file():
            dep_dir = dep_dir.parent
        if dep_dir in packages_by_dir:
            return packages_by_dir[dep_dir].name
        manifest = dep_dir / "Cargo.toml"
        if manifest.exists():
            dep_dir = dep_dir.resolve()
            if dep_dir in packages_by_dir:
                return packages_by_dir[dep_dir].name
    if actual_name in packages_by_name:
        return actual_name
    if key in packages_by_name:
        return key
    return None


def build_dependency_graph(packages: list[Package]) -> None:
    packages_by_name = {pkg.name: pkg for pkg in packages}
    packages_by_dir = {pkg.dir_path.resolve(): pkg for pkg in packages}
    workspace_cache: dict[Path, dict] = {}
    for pkg in packages:
        ws_manifest = None
        if pkg.workspace_root:
            ws_manifest = workspace_cache.setdefault(
                pkg.workspace_root, read_toml(pkg.workspace_root / "Cargo.toml")
            )
        dep_refs: list[DepRef] = []
        local_deps: list[str] = []
        external_deps: list[str] = []
        for kind, target, key, value in dependency_sections(pkg.manifest):
            actual_name, optional, path_str = normalize_dep_spec(key, value, ws_manifest)
            local_name = resolve_dep_local_name(
                key,
                actual_name,
                path_str,
                pkg.dir_path,
                packages_by_dir,
                packages_by_name,
            )
            dep_refs.append(
                DepRef(
                    key=key,
                    actual_name=actual_name,
                    kind=kind,
                    target=target,
                    optional=optional,
                    local_name=local_name,
                )
            )
            if local_name and local_name != pkg.name:
                local_deps.append(local_name)
            else:
                external_deps.append(actual_name)
        pkg.dep_refs = dep_refs
        pkg.direct_local_deps = unique(sorted(local_deps))
        pkg.external_deps = unique(sorted(external_deps))

    graph = {pkg.name: set(pkg.direct_local_deps) for pkg in packages}
    reverse_graph: dict[str, set[str]] = {pkg.name: set() for pkg in packages}
    for pkg, deps in graph.items():
        for dep in deps:
            reverse_graph.setdefault(dep, set()).add(pkg)

    for pkg in packages:
        seen: set[str] = set()
        queue = collections.deque(pkg.direct_local_deps)
        while queue:
            dep = queue.popleft()
            if dep in seen:
                continue
            seen.add(dep)
            for nxt in graph.get(dep, set()):
                if nxt not in seen:
                    queue.append(nxt)
        pkg.transitive_local_deps = sorted(seen - set(pkg.direct_local_deps))
        pkg.reverse_direct = sorted(reverse_graph.get(pkg.name, set()))

    for pkg in packages:
        seen: set[str] = set()
        queue = collections.deque(pkg.reverse_direct)
        while queue:
            dep = queue.popleft()
            if dep in seen:
                continue
            seen.add(dep)
            for nxt in reverse_graph.get(dep, set()):
                if nxt not in seen:
                    queue.append(nxt)
        pkg.reverse_transitive = sorted(seen - set(pkg.reverse_direct))


def make_package(manifest_path: Path) -> Package | None:
    manifest = read_toml(manifest_path)
    if "package" not in manifest:
        return None
    pkg_info = manifest["package"]
    pkg_dir = manifest_path.parent.resolve()
    rel_dir = pkg_dir.relative_to(REPO_ROOT).as_posix()
    workspace_root = find_workspace_root(pkg_dir)
    workspace_manifest = read_toml(workspace_root / "Cargo.toml") if workspace_root else None
    workspace_rel = (
        workspace_root.relative_to(REPO_ROOT).as_posix()
        if workspace_root and workspace_root != pkg_dir
        else (workspace_root.relative_to(REPO_ROOT).as_posix() if workspace_root else None)
    )
    lib_table = manifest.get("lib", {})
    has_lib = bool(lib_table) or (pkg_dir / "src/lib.rs").exists()
    lib_path = None
    if has_lib:
        lib_rel = lib_table.get("path", "src/lib.rs")
        lib_path = (pkg_dir / lib_rel).resolve()
        if not lib_path.exists():
            lib_path = None
    bin_paths: list[Path] = []
    bin_tables = manifest.get("bin", [])
    if isinstance(bin_tables, list):
        for item in bin_tables:
            if isinstance(item, dict) and item.get("path"):
                bin_paths.append((pkg_dir / item["path"]).resolve())
    if not bin_paths and (pkg_dir / "src/main.rs").exists():
        bin_paths.append((pkg_dir / "src/main.rs").resolve())
    has_bin = bool(bin_paths)
    build_rs = (pkg_dir / manifest.get("package", {}).get("build", "build.rs")).resolve()
    if not build_rs.exists():
        build_rs = None
    readme_path = choose_readme(pkg_dir, manifest)
    root_entry = lib_path or (bin_paths[0] if bin_paths else None)
    root_doc = extract_leading_doc(root_entry) if root_entry else ""
    category, role = classify_role(rel_dir)
    source_files = collect_source_files(pkg_dir)
    top_modules = extract_top_modules(root_entry) if root_entry else []
    top_module_files = (
        [
            mod_file
            for mod_file in (
                resolve_module_file(root_entry, mod_name) for mod_name, _ in top_modules[:8]
            )
            if mod_file
        ]
        if root_entry
        else []
    )
    module_descriptions = [
        describe_module(root_entry, mod_name, attrs)
        for mod_name, attrs in top_modules[:8]
    ] if root_entry else []
    scan_files = [root_entry] if root_entry else []
    scan_files.extend(top_module_files[:8])
    scan_files = [path for path in scan_files if path]
    if not scan_files:
        scan_files = source_files[: min(len(source_files), 20)]
    (
        public_structs,
        public_enums,
        public_traits,
        public_types,
        public_statics,
        public_functions,
    ) = extract_public_items(scan_files[: min(len(scan_files), 20)])
    init_scan_files = ([root_entry] if root_entry else []) + bin_paths[:1] + top_module_files[:4]
    init_scan_files = unique([path.as_posix() for path in init_scan_files if path])
    init_scan_files = [Path(path) for path in init_scan_files]
    if not init_scan_files:
        init_scan_files = scan_files[:6]
    init_functions = extract_init_functions(init_scan_files)
    integration, unit, examples, benches, fuzz = collect_test_layout(pkg_dir, source_files)
    is_proc_macro = bool(lib_table.get("proc-macro", False))
    return Package(
        name=pkg_info["name"],
        version=resolve_workspace_package_value(pkg_info, "version", workspace_manifest),
        description=resolve_workspace_package_value(pkg_info, "description", workspace_manifest),
        manifest_path=manifest_path.resolve(),
        dir_path=pkg_dir,
        rel_dir=rel_dir,
        manifest=manifest,
        workspace_root=workspace_root,
        workspace_rel=workspace_rel,
        is_proc_macro=is_proc_macro,
        has_lib=has_lib,
        has_bin=has_bin,
        lib_path=lib_path,
        bin_paths=bin_paths,
        build_rs=build_rs,
        readme_path=readme_path,
        root_doc=root_doc,
        category=category,
        role=role,
        source_files=source_files,
        top_modules=top_modules,
        module_descriptions=module_descriptions,
        public_structs=public_structs,
        public_enums=public_enums,
        public_traits=public_traits,
        public_types=public_types,
        public_statics=public_statics,
        public_functions=public_functions,
        init_functions=init_functions,
        path_keywords=[],
        heuristics=[],
        dep_refs=[],
        direct_local_deps=[],
        external_deps=[],
        transitive_local_deps=[],
        reverse_direct=[],
        reverse_transitive=[],
        integration_tests=integration,
        unit_test_files=unit,
        example_files=examples,
        bench_files=benches,
        fuzz_files=fuzz,
    )


def workspace_style(pkg: Package) -> str:
    if pkg.workspace_root:
        if pkg.workspace_root == REPO_ROOT:
            return "根工作区"
        return f"子工作区 `{pkg.workspace_rel}`"
    return "未识别为显式 workspace"


def describe_crate_kind(pkg: Package) -> str:
    if pkg.is_proc_macro:
        return "过程宏库"
    if pkg.has_lib and pkg.has_bin:
        return "库 + 二进制混合 crate"
    if pkg.has_lib:
        return "库 crate"
    if pkg.has_bin:
        return "二进制 crate"
    return "特殊布局 crate"


def doc_style(pkg: Package) -> str:
    if pkg.name in CURATED_DOCS:
        return "curated"
    if path_matches(pkg.rel_dir, "os/arceos/api/"):
        return "arceos_feature" if pkg.name in FEATURE_LIKE_CRATES else "arceos_api"
    if path_matches(pkg.rel_dir, "os/arceos/ulib/"):
        return "arceos_ulib"
    if path_matches(pkg.rel_dir, "os/arceos/examples/"):
        return "arceos_example"
    if path_matches(pkg.rel_dir, "test-suit/"):
        return "test_suite"
    if path_matches(pkg.rel_dir, "os/StarryOS/starryos/"):
        return "starry_entry"
    if path_matches(pkg.rel_dir, "components/axplat_crates/examples/"):
        return "platform_example"
    if pkg.is_proc_macro:
        return "proc_macro"
    if (
        path_matches(pkg.rel_dir, "xtask/")
        or path_matches(pkg.rel_dir, "scripts/")
        or path_matches(pkg.rel_dir, "os/arceos/tools/")
        or pkg.name == "cargo-axplat"
    ):
        return "host_tool"
    if (
        path_matches(pkg.rel_dir, "platform/")
        or path_matches(pkg.rel_dir, "components/axplat_crates/platforms/")
        or pkg.name == "ax-plat"
    ):
        return "platform"
    if path_matches(pkg.rel_dir, "os/arceos/modules/"):
        return "arceos_module"
    if pkg.rel_dir in {"components/axdevice", "components/axdevice_base"} or any(
        pkg.rel_dir.startswith(prefix) for prefix in HW_COMPONENT_PREFIXES
    ):
        return "hw_component"
    if pkg.category == "组件层":
        return "shared_component"
    return "generic"


def style_overview(pkg: Package) -> str | None:
    style = doc_style(pkg)
    mapping = {
        "arceos_api": "该 crate 更接近能力门面层：它把 `ax*` 内核模块的能力按 API 域重新组织成稳定接口，重点在 feature 转发、符号导出和上层契约，而不是独立实现完整子系统。",
        "arceos_feature": "该 crate 更像 ArceOS 的 feature 总开关或能力编排层，关键在编译期开关如何决定下游模块是否被装配进最终镜像。",
        "arceos_ulib": "该 crate 位于应用接口边界，重点是把底层模块能力包装成更接近 Rust `std` / libc 语义的用户态或应用开发接口。",
        "arceos_example": "该 crate 的实现通常很薄，核心价值不在抽象复用，而在于用最小代码路径把某个 ArceOS 能力组合真正跑起来。",
        "test_suite": "该 crate 的主线不是提供稳定库 API，而是构造可复现的系统级测试场景，并通过日志、退出行为或 QEMU 结果判断是否回归通过。",
        "starry_entry": "该 crate 是 StarryOS 的启动镜像/应用打包入口，复杂度主要体现在 rootfs、feature、启动参数和内核主包之间的装配关系。",
        "platform_example": "该 crate 更适合被理解为板级 bring-up 演示：重点不是抽象层次，而是最小平台初始化路径能否成立。",
        "proc_macro": "该 crate 应从宏入口、语法树解析和展开产物理解，运行时模块树通常不长，但编译期接口契约很关键。",
        "host_tool": "该 crate 运行在宿主机侧，重点是 CLI、配置、外部命令调用和开发流水线接线，而不是目标系统内核热路径。",
        "platform": "该 crate 的重心通常是板级假设、条件编译矩阵和启动时序，阅读时应优先关注架构/平台绑定点。",
        "hw_component": "该 crate 多数是寄存器级或设备级薄封装，复杂度集中在 MMIO 语义、安全假设和被上层平台/驱动整合的方式。",
        "shared_component": "该 crate 通常作为多个内核子系统共享的底层构件，重点在接口边界、数据结构和被上层复用的方式。",
    }
    return mapping.get(style)


def style_scenario(pkg: Package) -> str:
    style = doc_style(pkg)
    mapping = {
        "arceos_api": "面向 ArceOS 上层模块、用户库和应用接口层提供稳定能力门面，避免直接依赖过多内部模块细节。",
        "arceos_feature": "作为 ArceOS 的 feature 编排中心使用，用于把调度、网络、文件系统、设备等能力按需装配进最终镜像。",
        "arceos_ulib": "面向应用开发者提供 `std`/libc 风格接口，是应用与底层 `ax-api`/内核模块之间的主要边界层。",
        "arceos_example": "用于展示或回归某个具体 ArceOS 能力组合，既是示例程序，也是最小 smoke test 入口。",
        "test_suite": "用于验证固定功能点、特定 bug 回归或系统语义是否符合预期，通常通过 QEMU 日志或退出状态判断成功与否。",
        "starry_entry": "用于生成和运行 StarryOS 启动镜像，把 rootfs、内核 feature 和运行参数装配到完整系统入口中。",
        "platform_example": "用于演示 `axplat` 平台抽象的最小内核样例，便于验证中断、SMP、串口或启动路径。",
        "proc_macro": "供上游 crate 以属性宏、函数宏或派生宏形式调用，用来生成配置常量、接口绑定或样板代码。",
        "host_tool": "运行在宿主机侧，为构建、测试、镜像准备、依赖分析或开发辅助提供命令行能力。",
        "platform": "承担架构/板级适配职责，为上层运行时提供启动、中断、时钟、串口、设备树和内存布局等基础能力。",
        "hw_component": "提供寄存器定义、MMIO 访问或设备级操作原语，通常被平台 crate、驱动聚合层或更高层子系统进一步封装。",
        "shared_component": "作为共享基础设施被多个 OS 子系统复用，常见场景包括同步、内存管理、设备抽象、接口桥接和虚拟化基础能力。",
        "arceos_module": "主要服务于 ArceOS 内核模块装配，是运行时、驱动、内存、网络或同步等子系统的一部分。",
        "generic": "主要作为仓库中的专用支撑 crate 被上层组件调用。",
    }
    return mapping.get(style, "主要作为仓库中的专用支撑 crate 被上层组件调用。")


def style_command_block(pkg: Package) -> str | None:
    style = doc_style(pkg)
    if style == "arceos_example":
        cmd = f"cargo xtask arceos run --package {pkg.name} --arch riscv64"
        if any(key in pkg.name for key in ("httpclient", "httpserver")):
            cmd += " --net"
        elif "shell" in pkg.name:
            cmd += " --blk"
        return cmd
    if style == "test_suite":
        if path_matches(pkg.rel_dir, "test-suit/arceos/"):
            return (
                "cargo arceos test qemu --target riscv64gc-unknown-none-elf\n"
                f"cargo xtask arceos run --package {pkg.name} --arch riscv64"
            )
        return (
            "cargo starry test qemu --target riscv64\n"
            "cargo xtask starry run --arch riscv64 --package starryos-test"
        )
    if style == "starry_entry":
        return (
            "cargo xtask starry rootfs --arch riscv64\n"
            "cargo xtask starry run --arch riscv64 --package starryos"
        )
    if style == "platform_example":
        return f'cd "{pkg.rel_dir}" && make ARCH=<x86_64|aarch64|riscv64|loongarch64> run'
    if style == "host_tool":
        if path_matches(pkg.rel_dir, "xtask/"):
            return "cargo xtask <test|arceos|starry>"
        if path_matches(pkg.rel_dir, "os/arceos/tools/"):
            return f'cargo run --manifest-path "{pkg.rel_dir}/Cargo.toml"'
        if pkg.name == "cargo-axplat":
            return 'cd "components/axplat_crates" && cargo run -p cargo-axplat -- <subcommand>'
        if pkg.has_bin and not pkg.has_lib:
            if pkg.workspace_root == REPO_ROOT:
                return f"cargo run -p {pkg.name}"
            return f'cargo run --manifest-path "{pkg.rel_dir}/Cargo.toml"'
    if pkg.has_bin and not pkg.has_lib:
        if pkg.workspace_root == REPO_ROOT:
            return f"cargo run -p {pkg.name}"
        return f'cargo run --manifest-path "{pkg.rel_dir}/Cargo.toml"'
    return None


def dev_guide_title(pkg: Package) -> str:
    if doc_style(pkg) in {
        "arceos_example",
        "test_suite",
        "starry_entry",
        "platform_example",
        "host_tool",
    }:
        return "### 4.1 运行入口"
    return "### 4.1 依赖配置"


def package_summary(pkg: Package) -> str:
    style = doc_style(pkg)
    summary = pkg.description or pkg.root_doc or pkg.role
    if style == "test_suite":
        if path_matches(pkg.rel_dir, "test-suit/arceos/"):
            return "ArceOS 系统级测试与回归入口"
        return "StarryOS 系统级测试与回归入口"
    if style == "platform_example":
        return "基于 axplat 的平台 bring-up 示例内核"
    if style == "arceos_example" and summary == pkg.role:
        return "ArceOS 示例程序"
    if style == "host_tool" and not pkg.description:
        return pkg.role
    return summary


def dev_steps(pkg: Package) -> list[str]:
    style = doc_style(pkg)
    if style == "arceos_example":
        return [
            "先确认目标架构、平台和示例所需 feature；涉及网络或块设备时同步准备对应运行参数。",
            "优先通过 `cargo xtask arceos run --package <包名> --arch <arch>` 启动，而不是把它当作普通 host 程序直接运行。",
            "把串口输出、退出码和功能表现作为验证结果，必要时补充对应 `test-suit/arceos` 回归场景。",
        ]
    if style == "test_suite":
        test_cmd = (
            "`cargo arceos test qemu`"
            if path_matches(pkg.rel_dir, "test-suit/arceos/")
            else "`cargo starry test qemu`"
        )
        return [
            "先明确该测试场景对应的目标架构、QEMU 配置和成功/失败判据。",
            f"优先通过 {test_cmd} 跑完整测试入口，单包调试再退回 `run --package`。",
            "修改测试时同步检查日志匹配、预期 panic、退出状态和 feature 组合，保证回归结果可复现。",
        ]
    if style == "starry_entry":
        return [
            "先准备 rootfs、用户程序镜像和 StarryOS 目标架构配置。",
            "用 `cargo xtask starry rootfs --arch <arch>` 准备运行环境，再执行 `cargo xtask starry run --arch <arch>`。",
            "关注 init 进程、rootfs 挂载、syscall 行为和用户程序启动结果是否符合预期。",
        ]
    if style == "platform_example":
        return [
            "进入示例目录后用 `make ARCH=<arch> run` 触发最小内核演示，以验证平台抽象接线。",
            "必要时切换 `ARCH`/board 配置，观察串口、中断、SMP 等最小功能是否正常。",
            "若把示例迁移到新平台，优先保证启动、异常和控制台路径先成立，再扩展其他能力。",
        ]
    if style == "host_tool":
        return [
            "先确认该工具运行在宿主机侧，并准备需要的工作区、配置文件、镜像或外部命令环境。",
            "优先通过 CLI 子命令或 `--manifest-path` 方式运行，避免误把它当作裸机/内核镜像的一部分。",
            "对修改后的行为至少做一次成功路径和一次失败路径验证，重点检查日志、输出文件和外部命令返回值。",
        ]
    if style == "proc_macro":
        return [
            "在上游 crate 的 `Cargo.toml` 中添加该宏 crate 依赖。",
            "在类型定义、trait 接口或 API 注入点上应用宏，并核对输入语法是否满足宏约束。",
            "通过编译结果、展开代码和错误信息验证宏生成逻辑是否正确。",
        ]
    if style == "arceos_api":
        return [
            "优先通过该 crate 提供的稳定 API 接入能力，而不是直接深入底层 `os/arceos/modules/*` 实现。",
            "根据目标能力开启对应 feature，并确认它们与 `ax-runtime`、驱动、文件系统或网络子系统的装配关系。",
            "在最小消费者路径上验证 API 语义、错误码和资源释放行为是否与上层预期一致。",
        ]
    if style == "arceos_ulib":
        return [
            "将该 crate 视作应用接口层，先明确是走 `ax-std` 风格还是 libc/POSIX 风格接入。",
            "根据应用所需能力开启 feature，并确认与 `ax-api`/系统镜像配置保持一致。",
            "通过最小应用或示例程序验证线程、时间、I/O、文件系统或网络接口的语义是否正确。",
        ]
    if style == "platform":
        return [
            "先确认目标架构、板型和外设假设，再检查 feature/cfg 是否能选中正确的平台实现。",
            "修改平台代码时优先验证启动、串口、中断、时钟和内存布局这些 bring-up 基线能力。",
            "若涉及设备树或 MMIO 基址变化，需同步验证上层驱动和运行时是否仍能正确接线。",
        ]
    if style == "hw_component":
        return [
            "先明确该设备/寄存器组件的调用上下文，是被平台 crate 直接使用还是被驱动聚合层再次封装。",
            "修改寄存器位域、初始化顺序或中断相关逻辑时，应同步检查 `unsafe` 访问、访问宽度和副作用语义。",
            "尽量通过最小平台集成路径验证真实设备行为，而不要只依赖静态接口检查。",
        ]
    return [
        "在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。",
        "若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。",
        "在最小消费者路径上验证公开 API、错误分支与资源回收行为。",
    ]


def make_dependency_snippet(pkg: Package) -> str:
    cmd = style_command_block(pkg)
    style = doc_style(pkg)
    if style == "test_suite":
        return (
            "```toml\n"
            f"# `{pkg.name}` 主要作为测试/验证入口使用，通常不作为普通库依赖。\n"
            "# 推荐通过 xtask 统一测试入口或单包运行命令触发。\n"
            "```\n\n"
            "```bash\n"
            f"{cmd}\n"
            "```"
        )
    if cmd and style in {
        "arceos_example",
        "starry_entry",
        "platform_example",
        "host_tool",
    }:
        return textwrap.dedent(
            f"""\
            ```toml
            # `{pkg.name}` 是二进制/编排入口，通常不作为库依赖。
            # 更常见的接入方式是通过对应构建/运行命令触发，而不是在 Cargo.toml 中引用。
            ```

            ```bash
            {cmd}
            ```
            """
        ).rstrip()
    if pkg.has_bin and not pkg.has_lib:
        run_cmd = cmd or f"cargo run -p {pkg.name}"
        if path_matches(pkg.rel_dir, "os/axvisor/") and pkg.name == "axvisor":
            run_cmd = "cargo axvisor build"
        return textwrap.dedent(
            f"""\
            ```toml
            # `{pkg.name}` 是二进制/编排入口，通常不作为库依赖。
            # 更常见的接入方式是直接执行命令，而不是在 Cargo.toml 中引用。
            ```

            ```bash
            {run_cmd}
            ```
            """
        ).rstrip()
    return textwrap.dedent(
        f"""\
        ```toml
        [dependencies]
        {pkg.name} = {{ workspace = true }}

        # 如果在仓库外独立验证，也可以显式绑定本地路径：
        # {pkg.name} = {{ path = "{pkg.rel_dir}" }}
        ```
        """
    ).rstrip()


def format_feature_summary(pkg: Package) -> str:
    features = sorted(pkg.manifest.get("features", {}).keys())
    features = [f for f in features if f != "default"]
    if not features:
        return "该 crate 没有显式声明额外 Cargo feature，功能边界主要由模块本身决定。"
    shown, more = trim_list(features, 10)
    text = "、".join(f"`{x}`" for x in shown)
    if more:
        text += f" 等（另有 {more} 个 feature）"
    return f"主要通过 {text} 控制编译期能力装配。"


def format_public_api(pkg: Package) -> str:
    api_names = unique(
        pkg.public_functions[:8]
        + pkg.public_structs[:6]
        + pkg.public_enums[:4]
        + pkg.public_traits[:4]
    )
    if not api_names:
        if pkg.top_modules:
            mods = "、".join(f"`{mod}`" for mod, _ in pkg.top_modules[:6])
            return f"该 crate 更倾向按顶层模块组织接口，当前应重点关注 {mods} 等模块边界。"
        if pkg.has_bin and not pkg.has_lib:
            return "该 crate 的公开入口主要是 `main()` 或命令子流程，本身不强调稳定库 API。"
        if pkg.is_proc_macro:
            return "该 crate 的核心 API 体现在宏入口与属性/派生展开点，稳定接口以宏名和宏参数为主。"
        return "该 crate 的公开符号较少，更多承担内部桥接、配置注入或编排职责。"
    shown, more = trim_list(api_names, 10)
    text = "、".join(f"`{x}`" for x in shown)
    if more:
        text += f" 等（另有 {more} 个公开入口）"
    return f"从源码可见的主要公开入口包括 {text}。"


def format_call_chain(pkg: Package) -> str:
    style = doc_style(pkg)
    if style == "arceos_api":
        return "该 crate 没有单一固定的初始化链，通常按 CPU、时间、内存、任务、文件系统等能力域独立调用。"
    if style == "arceos_ulib":
        return "该 crate 没有单一固定的初始化链，常由应用按线程、时间、I/O、文件系统和网络等模块分别接入。"
    if style == "proc_macro":
        return "典型调用链发生在编译期：宏入口先解析 token/参数，再生成目标 crate 需要的常量、实现或辅助代码。"
    funcs = pkg.init_functions[:]
    if pkg.has_bin and "main" in funcs:
        funcs = ["main"] + [name for name in funcs if name != "main"]
    funcs = funcs[:6]
    if not funcs:
        if pkg.has_bin and not pkg.has_lib:
            return "典型调用链以 `main()` 为起点，向下串接配置解析、初始化和运行控制逻辑。"
        return "该 crate 没有单一固定的初始化链，通常由上层调用者按 feature/trait 组合接入。"
    chain = " -> ".join(f"`{name}()`" for name in funcs[:5])
    if len(funcs) > 5:
        chain += " -> ..."
    return f"按当前源码布局，常见入口/初始化链可概括为 {chain}。"


def format_data_structure_summary(pkg: Package) -> str:
    key_objects = [
        item
        for item in unique(pkg.public_structs[:6] + pkg.public_enums[:4] + pkg.public_types[:4] + pkg.public_statics[:4])
        if re.search(r"[A-Z]", item) or item.isupper()
    ]
    if not key_objects:
        if pkg.is_proc_macro:
            return "关键“结构”更多体现在编译期语法树节点、宏输入 token 流和展开规则上。"
        return "该 crate 暴露的数据结构较少，关键复杂度主要体现在模块协作、trait 约束或初始化时序。"
    shown, more = trim_list(key_objects, 10)
    text = "、".join(f"`{x}`" for x in shown)
    if more:
        text += f" 等（另有 {more} 个关键类型/对象）"
    return f"可直接观察到的关键数据结构/对象包括 {text}。"


def make_mermaid(pkg: Package) -> str:
    lines = ["```mermaid", "graph LR"]
    lines.append(f'    current["{pkg.name}"]')
    for dep in pkg.direct_local_deps[:8]:
        lines.append(f'    current --> {safe_mermaid_id(dep)}["{dep}"]')
    for user in pkg.reverse_direct[:8]:
        lines.append(f'    {safe_mermaid_id(user)}["{user}"] --> current')
    lines.append("```")
    return "\n".join(lines)


def safe_mermaid_id(name: str) -> str:
    return re.sub(r"[^A-Za-z0-9_]", "_", name)


def describe_project_position(pkg: Package, project: str, packages_by_name: dict[str, Package]) -> str:
    project_labels = {
        "arceos": ("ArceOS", "os/arceos/"),
        "starryos": ("StarryOS", "os/StarryOS/"),
        "axvisor": ("Axvisor", "os/axvisor/"),
    }
    label, prefix = project_labels[project]
    if path_matches(pkg.rel_dir, prefix):
        return f"`{pkg.name}` 直接位于 `{prefix}` 目录树中，是 {label} 工程本体的一部分，承担 {pkg.role}。"
    direct = [
        dep for dep in pkg.reverse_direct if project_of_package(packages_by_name[dep]) == project
    ]
    transitive = [
        dep for dep in pkg.reverse_transitive if project_of_package(packages_by_name[dep]) == project
    ]
    if direct:
        return (
            f"`{pkg.name}` 不在 {label} 目录内部，但被 {format_inline_list(direct, '无', 6)} "
            f"等 {label} crate 直接依赖，说明它是该系统的共享构件或底层服务。"
        )
    if transitive:
        return (
            f"`{pkg.name}` 主要通过 {format_inline_list(transitive, '无', 6)} "
            f"等上层 crate 被 {label} 间接复用，通常处于更底层的公共依赖层。"
        )
    if project == "axvisor" and (
        "vm" in pkg.name or "vcpu" in pkg.name or "axvisor" in pkg.name
    ):
        return f"从命名与目录角色判断，`{pkg.name}` 与虚拟化栈语义高度相关，但当前仓库中未检测到更多 {label} Rust crate 直接消费它。"
    if project == "starryos" and pkg.name.startswith("starry"):
        return f"`{pkg.name}` 虽不一定位于 `os/StarryOS/` 内部，但从命名与职责上属于 StarryOS 兼容栈的重要组件。"
    if project == "arceos" and pkg.name.startswith("ax"):
        return f"`{pkg.name}` 更偏 ArceOS 生态的基础设施或公共模块；当前未观察到 {label} 本体对其存在显式直接依赖。"
    return f"当前未检测到 {label} 工程本体对 `{pkg.name}` 的显式本地依赖，若参与该系统，通常经外部工具链、配置或更底层生态间接体现。"


def test_strategy(pkg: Package) -> tuple[str, str, str, str]:
    current: list[str] = []
    style = doc_style(pkg)
    if pkg.integration_tests:
        current.append(f"存在 crate 内集成测试：{format_inline_list(pkg.integration_tests, '无', 6)}。")
    if pkg.unit_test_files:
        current.append(f"存在单元测试/`#[cfg(test)]` 场景：{format_inline_list(pkg.unit_test_files, '无', 6)}。")
    if pkg.example_files:
        current.append(f"存在示例程序：{format_inline_list(pkg.example_files, '无', 6)}，可作为冒烟验证入口。")
    if pkg.bench_files:
        current.append(f"存在基准测试：{format_inline_list(pkg.bench_files, '无', 6)}。")
    if pkg.fuzz_files:
        current.append(f"存在模糊测试入口：{format_inline_list(pkg.fuzz_files, '无', 6)}。")
    if style == "test_suite":
        current.append("该 crate 本身就是系统级测试入口，测试价值主要来自 QEMU/平台运行结果而非 host 侧库测试。")
    if not current:
        current.append("当前 crate 目录中未发现显式 `tests/`/`benches/`/`fuzz/` 入口，更可能依赖上层系统集成测试或跨 crate 回归。")

    if style in {"shared_component", "generic"}:
        unit_focus = "建议用单元测试覆盖公开 API、错误分支、边界条件以及并发/内存安全相关不变量。"
        integration_focus = "建议补充被 ArceOS/StarryOS/Axvisor 消费时的最小集成路径，确保接口语义与 feature 组合稳定。"
        coverage = "覆盖率建议：核心算法与错误路径达到高覆盖，关键数据结构和边界条件应实现接近完整覆盖。"
    elif style in {"arceos_api", "arceos_feature", "arceos_module"}:
        unit_focus = "建议围绕 API 契约、feature 分支、资源管理和错误恢复路径编写单元测试。"
        integration_focus = "建议至少补一条 ArceOS 示例或 `test-suit/arceos` 路径，必要时覆盖多架构或多 feature 组合。"
        coverage = "覆盖率建议：公开 API、初始化失败路径和主要 feature 组合必须覆盖；涉及调度/内存/设备时需补系统级验证。"
    elif style == "arceos_ulib":
        unit_focus = "建议覆盖 std/libc 风格包装层的语义映射、错误码转换和 feature 分支。"
        integration_focus = "建议用最小应用、示例程序和系统镜像运行验证线程、I/O、时间、文件系统和网络接口语义。"
        coverage = "覆盖率建议：对外暴露的高层 API 需要稳定覆盖；与底层子系统交互的关键路径应至少有一条端到端验证。"
    elif style == "arceos_example":
        unit_focus = "示例 crate 通常不以大量单元测试为主，若存在辅助函数，可覆盖参数解析、状态检查和错误分支。"
        integration_focus = "重点是通过 `cargo xtask arceos run` 在 QEMU/目标平台上验证示例行为与输出是否符合预期。"
        coverage = "覆盖率建议以场景覆盖为主：至少保证示例主路径、关键 feature 组合和失败输出可观测。"
    elif style == "test_suite":
        unit_focus = "若该 crate 含辅助库逻辑，可对断言解析、日志匹配和测试输入构造做单元测试。"
        integration_focus = "重点是保持测试矩阵可复现：架构、平台、QEMU 配置、成功/失败判据都应纳入回归。"
        coverage = "覆盖率要求以场景覆盖为主：应覆盖正常路径、预期失败路径和关键平台/feature 组合。"
    elif style == "starry_entry" or pkg.category == "StarryOS 层":
        unit_focus = "建议围绕 syscall 语义、进程/线程状态转换、地址空间或信号处理分支做单元测试。"
        integration_focus = "建议结合 rootfs、用户程序加载和 `test-suit/starryos` 做端到端回归，验证 Linux 兼容行为。"
        coverage = "覆盖率建议：syscall 分发、关键状态机和错误码映射应覆盖主要正常/异常路径；复杂场景需以集成测试补齐。"
    elif pkg.category == "Axvisor 层":
        unit_focus = "建议围绕配置解析、VM/vCPU 状态迁移、宿主接口注入和错误恢复路径做单元测试。"
        integration_focus = "建议通过 QEMU、Guest 镜像和多板级配置做集成验证，关注启动、暂停、停止和异常退出。"
        coverage = "覆盖率建议：配置装载、状态迁移和 VM Exit 热路径需要重点覆盖；涉及硬件行为时以 QEMU/板级测试为准。"
    elif style == "host_tool":
        unit_focus = "建议覆盖命令解析、配置序列化/反序列化、路径计算和失败分支。"
        integration_focus = "建议增加 CLI 金丝雀测试、示例工程 smoke test 或与 CI 命令一致的端到端验证。"
        coverage = "覆盖率建议：命令分派和配置读写逻辑应保持高覆盖，外部命令执行路径至少要有成功/失败双向验证。"
    elif style == "proc_macro":
        unit_focus = "建议覆盖语法树解析、输入约束检查和展开代码生成逻辑。"
        integration_focus = "建议增加 compile-pass / compile-fail 或 UI 测试，验证宏在真实调用 crate 中的展开行为。"
        coverage = "覆盖率建议：宏入口、错误诊断和关键展开分支需要重点覆盖，必要时结合快照测试检查生成代码。"
    elif style in {"platform", "platform_example"}:
        unit_focus = "若存在纯函数或配置辅助逻辑，可覆盖地址布局计算、设备树解析和平台参数选择分支。"
        integration_focus = "重点验证启动、串口、中断、时钟和内存布局等 bring-up 基线能力，必要时覆盖多板级/多架构。"
        coverage = "覆盖率建议以平台场景覆盖为主：至少确保一条真实启动链贯通，并覆盖关键 cfg/feature 组合。"
    elif style == "hw_component":
        unit_focus = "建议覆盖寄存器位域、设备状态转换、边界参数和 `unsafe` 访问前提。"
        integration_focus = "建议结合最小平台或驱动集成路径验证真实设备行为，重点检查初始化、中断和收发等主线。"
        coverage = "覆盖率建议：寄存器访问辅助函数和关键状态机保持高覆盖；真实硬件语义以集成验证补齐。"
    else:
        unit_focus = "建议覆盖公开 API、状态转换和异常分支。"
        integration_focus = "建议补充最小消费者路径，验证该 crate 在真实调用链中可用。"
        coverage = "覆盖率建议：公开 API、边界条件和关键错误处理路径需要显式覆盖。"

    return "\n".join(f"- {line}" for line in current), unit_focus, integration_focus, coverage


def make_architecture_section(pkg: Package) -> str:
    module_lines = format_bullet_lines(
        pkg.module_descriptions,
        "当前 crate 未显式声明多个顶层 `mod`，复杂度更可能集中在单文件入口、宏展开或下层子 crate。",
    )
    mechanisms = "\n".join(f"- {item}" for item in pkg.heuristics) if pkg.heuristics else "- 实现重心偏向接口组织和模块协作。"
    source_layout = [
        f"- 目录角色：{pkg.role}",
        f"- crate 形态：{describe_crate_kind(pkg)}",
        f"- 工作区位置：{workspace_style(pkg)}",
        f"- feature 视角：{format_feature_summary(pkg)}",
        f"- 关键数据结构：{format_data_structure_summary(pkg)}",
    ]
    overview = style_overview(pkg)
    if overview:
        source_layout.append(f"- 设计重心：{overview}")
    return "\n".join(source_layout) + "\n\n### 1.1 内部模块划分\n" + module_lines + "\n\n### 1.2 核心算法/机制\n" + mechanisms


def make_function_section(pkg: Package) -> str:
    style = doc_style(pkg)
    scenario = style_scenario(pkg)
    usage_note = ""
    if style == "proc_macro":
        usage_note = " 这类接口往往不是运行时函数调用，而是编译期宏展开点。"
    elif style in {"arceos_example", "test_suite", "platform_example"}:
        usage_note = " 这类 crate 的核心使用方式通常是运行入口本身，而不是被别的库当作稳定 API 依赖。"
    bullets = [
        f"- 功能定位：{package_summary(pkg)}",
        f"- 对外接口：{format_public_api(pkg)}",
        f"- 典型使用场景：{scenario}{usage_note}",
        f"- 关键调用链示例：{format_call_chain(pkg)}",
    ]
    return "\n".join(bullets)


def make_dependency_section(pkg: Package) -> str:
    dep_lines = [
        make_mermaid(pkg),
        "",
        "### 3.1 直接与间接依赖",
        format_markdown_list(
            pkg.direct_local_deps,
            "未检测到本仓库内的直接本地依赖；该 crate 可能主要依赖外部生态或承担叶子节点角色。",
        ),
        "",
        "### 3.2 间接本地依赖",
        format_markdown_list(
            pkg.transitive_local_deps,
            "未检测到额外的间接本地依赖，或依赖深度主要停留在第一层。",
        ),
        "",
        "### 3.3 被依赖情况",
        format_markdown_list(
            pkg.reverse_direct,
            "当前未发现本仓库内其他 crate 对其存在直接本地依赖。",
        ),
        "",
        "### 3.4 间接被依赖情况",
        format_markdown_list(
            pkg.reverse_transitive,
            "当前未发现更多间接消费者，或该 crate 主要作为终端入口使用。",
        ),
        "",
        "### 3.5 关键外部依赖",
        format_markdown_list(
            pkg.external_deps,
            "当前依赖集合几乎完全来自仓库内本地 crate。",
        ),
    ]
    return "\n".join(dep_lines)


def make_dev_guide(pkg: Package) -> str:
    style = doc_style(pkg)
    steps = dev_steps(pkg)
    api_hints = []
    if style == "proc_macro":
        api_hints.append("应优先识别宏名、输入语法约束和展开后会生成哪些符号，而不是只看辅助函数名。")
    if style in {"arceos_example", "test_suite", "platform_example", "host_tool"}:
        api_hints.append("该 crate 的关键接入点通常是运行命令、CLI 参数或入口函数，而不是稳定库 API。")
    if pkg.public_functions:
        api_hints.append(f"优先关注函数入口：{format_inline_list(pkg.public_functions, '无', 8)}。")
    if pkg.public_structs:
        api_hints.append(f"上下文/对象类型通常从 {format_inline_list(pkg.public_structs, '无', 6)} 等结构开始。")
    if not api_hints:
        api_hints.append("该 crate 更偏编排、配置或内部 glue 逻辑，关键使用点通常体现在 feature、命令或入口函数上。")
    return (
        dev_guide_title(pkg) + "\n"
        + make_dependency_snippet(pkg)
        + "\n\n### 4.2 初始化流程\n"
        + "\n".join(f"{idx}. {step}" for idx, step in enumerate(steps, start=1))
        + "\n\n### 4.3 关键 API 使用提示\n"
        + "\n".join(f"- {item}" for item in api_hints)
    )


def make_cross_project_section(pkg: Package, packages_by_name: dict[str, Package]) -> str:
    return "\n".join(
        [
            f"### 6.1 ArceOS\n{describe_project_position(pkg, 'arceos', packages_by_name)}",
            "",
            f"### 6.2 StarryOS\n{describe_project_position(pkg, 'starryos', packages_by_name)}",
            "",
            f"### 6.3 Axvisor\n{describe_project_position(pkg, 'axvisor', packages_by_name)}",
        ]
    )


def package_doc(pkg: Package, packages_by_name: dict[str, Package]) -> str:
    current_tests, unit_focus, integration_focus, coverage = test_strategy(pkg)
    summary = package_summary(pkg)
    readme_note = (
        f"`{pkg.readme_path.relative_to(REPO_ROOT).as_posix()}`"
        if pkg.readme_path
        else "未检测到 crate 层 README"
    )
    lines = [
        f"# `{pkg.name}` 技术文档",
        "",
        f"> 路径：`{pkg.rel_dir}`",
        f"> 类型：{describe_crate_kind(pkg)}",
        f"> 分层：{pkg.category} / {pkg.role}",
        f"> 版本：`{pkg.version}`",
        f"> 文档依据：当前仓库源码、`Cargo.toml` 与 {readme_note}",
        "",
        f"`{pkg.name}` 的核心定位是：{summary}",
        "",
        "## 1. 架构设计分析",
        make_architecture_section(pkg),
        "",
        "## 2. 核心功能说明",
        make_function_section(pkg),
        "",
        "## 3. 依赖关系图谱",
        make_dependency_section(pkg),
        "",
        "## 4. 开发指南",
        make_dev_guide(pkg),
        "",
        "## 5. 测试策略",
        "### 5.1 当前仓库内的测试形态",
        current_tests,
        "",
        "### 5.2 单元测试重点",
        f"- {unit_focus}",
        "",
        "### 5.3 集成测试重点",
        f"- {integration_focus}",
        "",
        "### 5.4 覆盖率要求",
        f"- {coverage}",
        "",
        "## 6. 跨项目定位分析",
        make_cross_project_section(pkg, packages_by_name),
        "",
    ]
    return "\n".join(lines).rstrip() + "\n"


def index_doc(packages: list[Package]) -> str:
    counter = collections.Counter(pkg.category for pkg in packages)
    lines = [
        "# Crate 技术文档总览",
        "",
        f"当前仓库共识别到 **{len(packages)}** 个带 `[package]` 的 Rust crate。本文档索引与 `docs/crates/*.md` 一起构成按 crate 维度的技术参考集合。",
        "",
        "## 分类统计",
        "",
    ]
    for category, count in sorted(counter.items()):
        lines.append(f"- {category}：`{count}` 个")
    lines.extend(
        [
            "",
            "## 文档索引",
            "",
            "| Crate | 分类 | 路径 | 直接本地依赖 | 直接被依赖 | 文档 |",
            "| --- | --- | --- | ---: | ---: | --- |",
        ]
    )
    for pkg in packages:
        lines.append(
            "| "
            f"`{pkg.name}` | {pkg.category} | `{pkg.rel_dir}` | {len(pkg.direct_local_deps)} | {len(pkg.reverse_direct)} | "
            f"[查看](./{pkg.name}.md) |"
        )
    lines.extend(
        [
            "",
            "## 使用建议",
            "",
            "- 若要理解系统分层，建议先阅读与自己目标系统最接近的 crate 文档，再沿“直接被依赖”列表向上追踪。",
            "- 若要做底层修改，建议先看组件层 crate 的文档，再检查其在 ArceOS、StarryOS、Axvisor 中的跨项目定位段落。",
            "- 本目录文档依据源码静态分析自动整理；涉及 feature 条件编译、QEMU 行为和外部镜像配置时，应与对应系统总文档联合阅读。",
            "",
        ]
    )
    return "\n".join(lines)


def write_docs(packages: list[Package], selected: set[str] | None) -> None:
    DOCS_ROOT.mkdir(parents=True, exist_ok=True)
    packages_by_name = {pkg.name: pkg for pkg in packages}
    selected_packages = [pkg for pkg in packages if not selected or pkg.name in selected]
    if not selected:
        selected_packages = [pkg for pkg in selected_packages if pkg.name not in CURATED_DOCS]
    for pkg in selected_packages:
        content = package_doc(pkg, packages_by_name)
        (DOCS_ROOT / f"{pkg.name}.md").write_text(content, encoding="utf-8")
    if not selected:
        (DOCS_ROOT / "README.md").write_text(index_doc(packages), encoding="utf-8")


def build_packages() -> list[Package]:
    packages = [make_package(path) for path in iter_cargo_manifests(REPO_ROOT)]
    packages = [pkg for pkg in packages if pkg is not None]
    packages.sort(key=lambda pkg: pkg.name)
    build_dependency_graph(packages)
    for pkg in packages:
        pkg.path_keywords = detect_keywords(pkg)
        pkg.heuristics = summarize_mechanisms(pkg)
    return packages


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate crate technical docs")
    parser.add_argument(
        "--packages",
        nargs="*",
        help="Only generate docs for the given package names",
    )
    args = parser.parse_args()
    packages = build_packages()
    selected = set(args.packages) if args.packages else None
    write_docs(packages, selected)
    generated = len(selected) if selected else len([pkg for pkg in packages if pkg.name not in CURATED_DOCS])
    print(f"Generated {generated} crate docs under {DOCS_ROOT.relative_to(REPO_ROOT)}")


if __name__ == "__main__":
    main()
