#!/usr/bin/env python3
"""分析 tgoskits 137 个 crate 的仓库内直接依赖，自底向上分层，生成 docs/tgoskits-dependency.md。"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict, deque
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "scripts"))
from gen_crate_docs import REPO_ROOT as DOC_ROOT, build_packages, classify_role
assert DOC_ROOT == REPO_ROOT

EXTERNAL_CATEGORIES = [
    ("序列化/数据格式", ["serde", "toml", "json", "base64", "hex", "bincode", "byteorder", "bytes"]),
    ("异步/并发", ["tokio", "futures", "async", "crossbeam", "parking_lot", "rayon"]),
    ("网络/协议", ["http", "hyper", "axum", "tower", "rustls", "smoltcp", "socket2", "mio"]),
    ("加密/安全", ["digest", "sha", "rand", "aead", "ring", "rsa", "aes", "hmac"]),
    ("日志/错误", ["log", "tracing", "anyhow", "thiserror", "env_logger"]),
    ("命令行/配置", ["clap", "argh", "bitflags", "semver", "cargo_metadata"]),
    ("系统/平台", ["libc", "cc", "cmake", "linux-raw-sys", "rustix", "nix", "windows", "memchr"]),
    ("宏/代码生成", ["syn", "quote", "proc-macro", "derive", "paste", "darling", "heck"]),
    ("嵌入式/裸机", ["cortex-m", "embedded", "tock-registers", "critical-section", "defmt"]),
    ("数据结构/算法", ["hashbrown", "indexmap", "smallvec", "arrayvec", "bitvec", "lru"]),
    ("设备树/固件", ["fdt", "xmas-elf", "kernel-elf", "multiboot", "fitimage"]),
    ("工具库/其他", []),
]
LAYER0 = "基础层（无仓库内直接依赖）"
MERMAID_CLASS = {
    "组件层": "cat_comp", "ArceOS 层": "cat_arceos", "StarryOS 层": "cat_starry",
    "Axvisor 层": "cat_axvisor", "平台层": "cat_plat", "工具层": "cat_tool",
    "测试层": "cat_test", "其他": "cat_misc",
}
CLASS_DEF = """
    classDef cat_comp fill:#e3f2fd,stroke:#1565c0,stroke-width:2px
    classDef cat_arceos fill:#e8f5e9,stroke:#2e7d32,stroke-width:2px
    classDef cat_starry fill:#fce4ec,stroke:#c2185b,stroke-width:2px
    classDef cat_axvisor fill:#e1f5fe,stroke:#01579b,stroke-width:2px
    classDef cat_plat fill:#f3e5f5,stroke:#6a1b9a,stroke-width:2px
    classDef cat_tool fill:#fff8e1,stroke:#f57f17,stroke-width:2px
    classDef cat_test fill:#efebe9,stroke:#5d4037,stroke-width:2px
    classDef cat_misc fill:#eceff1,stroke:#455a64,stroke-width:2px
"""


def mid(s: str) -> str:
    x = re.sub(r"[^a-zA-Z0-9_]", "_", s)
    return ("n_" + x) if x and x[0].isdigit() else (x or "empty")


def internal_graph(pkgs):
    names = {p.name for p in pkgs}
    succ = {p.name: [d for d in p.direct_local_deps if d in names and d != p.name] for p in pkgs}
    return succ, names


def tarjan(nodes, succ):
    idx, st, on = 0, [], set()
    indices, low = {}, {}
    sccs = []

    def sc(v):
        nonlocal idx
        indices[v] = low[v] = idx
        idx += 1
        st.append(v)
        on.add(v)
        for w in succ.get(v, []):
            if w not in indices:
                sc(w)
                low[v] = min(low[v], low[w])
            elif w in on:
                low[v] = min(low[v], indices[w])
        if low[v] == indices[v]:
            comp = []
            while True:
                w = st.pop()
                on.remove(w)
                comp.append(w)
                if w == v:
                    break
            sccs.append(comp)

    for v in sorted(nodes):
        if v not in indices:
            sc(v)
    return sccs


def layers_from_scc(nodes, succ):
    sccs = tarjan(nodes, succ)
    scc_of = {n: i for i, c in enumerate(sccs) for n in c}
    n = len(sccs)
    inc = [set() for _ in range(n)]
    out = [set() for _ in range(n)]
    for u in nodes:
        for w in succ.get(u, []):
            iu, iw = scc_of[u], scc_of[w]
            if iu != iw:
                inc[iu].add(iw)
                out[iw].add(iu)
    indeg = [len(inc[i]) for i in range(n)]
    q = deque([i for i in range(n) if indeg[i] == 0])
    order = []
    while q:
        i = q.popleft()
        order.append(i)
        for j in out[i]:
            indeg[j] -= 1
            if indeg[j] == 0:
                q.append(j)
    sl = [0] * n
    for i in order:
        sl[i] = 0 if not inc[i] else 1 + max(sl[j] for j in inc[i])
    return {name: sl[scc_of[name]] for name in nodes}, sccs


def parse_lock_dep_line(raw: str) -> tuple[str, str]:
    """Cargo.lock dependency 字符串：'name version' 或附带 ' (registry+...)'。"""
    base = raw.split(" (", 1)[0].strip()
    parts = base.split()
    if len(parts) >= 2:
        return " ".join(parts[:-1]), parts[-1]
    return base, ""


def parse_lock_full(path: Path) -> list[dict]:
    content = path.read_text(encoding="utf-8")
    pkgs: list[dict] = []
    for block in re.split(r"\n\n+", content):
        if "[[package]]" not in block:
            continue
        nm = re.search(r'^name\s*=\s*"([^"]+)"', block, re.M)
        vm = re.search(r'^version\s*=\s*"([^"]+)"', block, re.M)
        sm = re.search(r'^source\s*=\s*"([^"]+)"', block, re.M)
        if not nm or not vm:
            continue
        ds = re.search(r"^dependencies\s*=\s*\[(.*?)\]", block, re.M | re.S)
        deps_raw: list[str] = []
        if ds:
            deps_raw = re.findall(r'"([^"]+)"', ds.group(1))
        pkgs.append(
            {
                "name": nm.group(1),
                "version": vm.group(1),
                "source": sm.group(1) if sm else None,
                "deps_raw": deps_raw,
            }
        )
    return pkgs


def lock_stats(lock_path: Path, local_names: set):
    pkgs = parse_lock_full(lock_path)
    ext = [p for p in pkgs if p["source"] is not None]
    cats = defaultdict(list)
    for p in ext:
        low = p["name"].lower()
        cat = "工具库/其他"
        for c, kws in EXTERNAL_CATEGORIES[:-1]:
            if any(k in low for k in kws):
                cat = c
                break
        cats[cat].append(f"{p['name']} {p['version']}")
    for c in cats:
        cats[c].sort()
    return {
        "lock_total": len(pkgs),
        "internal_in_lock": sum(
            1 for p in pkgs if p["source"] is None and p["name"] in local_names
        ),
        "external_crates": len(ext),
        "external_cats": dict(cats),
    }


def cargo_package_descriptions() -> dict[tuple[str, str], str]:
    """从 cargo metadata 取 description，键为 (name, version)。"""
    try:
        r = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            timeout=300,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        print(f"进度: cargo metadata 不可用（{e}），外部简介填 —", file=sys.stderr)
        return {}
    if r.returncode != 0:
        print("进度: cargo metadata 非零退出，外部简介填 —", file=sys.stderr)
        return {}
    try:
        data = json.loads(r.stdout)
    except json.JSONDecodeError:
        return {}
    out: dict[tuple[str, str], str] = {}
    for pkg in data.get("packages", []):
        d = (pkg.get("description") or "").strip()
        if not d:
            continue
        d = re.sub(r"\s+", " ", d).replace("|", "｜")
        out[(pkg["name"], pkg["version"])] = d
    return out


def categorize_external_name(name: str) -> str:
    low = name.lower()
    for c, kws in EXTERNAL_CATEGORIES[:-1]:
        if any(k in low for k in kws):
            return c
    return "工具库/其他"


def lock_section5_category_tables_md(
    lock_path: Path, local_names: set[str], desc_map: dict[tuple[str, str], str]
) -> list[str]:
    pkgs = parse_lock_full(lock_path)

    ext_versions_by_name: dict[str, list[str]] = defaultdict(list)
    for p in pkgs:
        if p["source"] is not None:
            ext_versions_by_name[p["name"]].append(p["version"])

    # 内部工作区包（Lock 中无 source）且属于 137：dependencies 中直接指向外部的边
    ext_key_consumers: dict[tuple[str, str], set[str]] = defaultdict(set)
    for p in pkgs:
        if p["source"] is not None:
            continue
        if p["name"] not in local_names:
            continue
        for raw in p["deps_raw"]:
            dn, dv = parse_lock_dep_line(raw)
            if not dn or dn in local_names:
                continue
            if dv:
                ext_key_consumers[(dn, dv)].add(p["name"])
            else:
                # Lock 中常出现无版本号的依赖项（如 "log",），需挂到该名的各外部版本行上
                for ver in ext_versions_by_name.get(dn, []):
                    ext_key_consumers[(dn, ver)].add(p["name"])

    # 外部包块：其 dependencies 中指向 137 清单的直接边
    ext_key_internals: dict[tuple[str, str], list[str]] = {}
    for p in pkgs:
        if p["source"] is None:
            continue
        key = (p["name"], p["version"])
        ints: set[str] = set()
        for raw in p["deps_raw"]:
            dn, _ = parse_lock_dep_line(raw)
            if dn in local_names:
                ints.add(dn)
        ext_key_internals[key] = sorted(ints)

    seen: set[tuple[str, str]] = set()
    by_cat: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for p in pkgs:
        if p["source"] is None:
            continue
        key = (p["name"], p["version"])
        if key in seen:
            continue
        seen.add(key)
        by_cat[categorize_external_name(p["name"])].append(key)
    for c in by_cat:
        by_cat[c].sort()

    def intro_cell(name: str, ver: str) -> str:
        d = desc_map.get((name, ver), "")
        if not d:
            return "—"
        if len(d) > 100:
            return d[:99] + "…"
        return d

    lines = [
        "## 5. Lock 外部依赖（关键词粗分）",
        "",
        "按 crate **名称**关键词粗分类；**内部组件**为本文扫描到的 137 个仓库 crate。",
        "关系统计来自根目录 **Cargo.lock** 各 `[[package]]` 的 `dependencies` 列表，仅统计**直接**依赖。",
        "简介来自 `cargo metadata` 的 `description`（≤100 字）；无数据或 metadata 失败时为 —。",
        "",
        "| 类别 | 外部包条目数（去重 name+version） |",
        "|------|-------------------------------------|",
    ]
    for cat in sorted(by_cat.keys(), key=lambda x: (-len(by_cat[x]), x)):
        lines.append(f"| {cat} | {len(by_cat[cat])} |")

    for cat in sorted(by_cat.keys()):
        lines += ["", f"#### {cat}", ""]
        lines += [
            "| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |",
            "|--------------------------|----------------|---------------------------|---------------------------|",
        ]
        for name, ver in by_cat[cat]:
            key = (name, ver)
            cons = sorted(ext_key_consumers.get(key, set()))
            ints = ext_key_internals.get(key, [])
            intro = intro_cell(name, ver)
            nv = f"`{name}` `{ver}`"
            lines.append(
                f"| {nv} | {intro} | {fmt_crate_list(cons)} | {fmt_crate_list(ints)} |"
            )
        lines.append("")
    return lines


def mermaid_layers(maxL, byL):
    pal = [("#eceff1", "#455a64"), ("#e8f5e9", "#2e7d32"), ("#fff9c4", "#f9a825"), ("#ffe0b2", "#ef6c00"),
           ("#e1bee7", "#6a1b9a"), ("#ffcdd2", "#c62828"), ("#b2ebf2", "#00838f"), ("#f8bbd0", "#c2185b")]
    lines = ["```mermaid", "flowchart TB", "    direction TB"]
    for L in range(maxL, -1, -1):
        pk = sorted(byL.get(L, []))
        brief = "、".join(f"`{x}`" for x in pk[:20]) + (f" …共{len(pk)}个" if len(pk) > 20 else "")
        ln = LAYER0 if L == 0 else "堆叠层（依赖更底层 crate）"
        lines += [f'    L{L}["<b>层级 {L}</b><br/>{ln}<br/>{brief}"]',
                  f"    classDef ls{L} fill:{pal[L%len(pal)][0]},stroke:{pal[L%len(pal)][1]},stroke-width:2px,color:#000",
                  f"    class L{L} ls{L}"]
    for L in range(maxL, 0, -1):
        lines.append(f"    L{L} --> L{L-1}")
    lines.append("```")
    return "\n".join(lines)


def fmt_crate_list(names: list[str]) -> str:
    if not names:
        return "—"
    return " ".join(f"`{n}`" for n in names)


def brief_intro(pkg, max_chars: int = 50) -> str:
    """不超过 max_chars 个字符的简介：优先 Cargo description，其次 crate 文档摘要，再次路径启发角色。"""
    text = (pkg.description or "").strip()
    if not text:
        text = (pkg.root_doc or "").strip()
    if not text:
        _, role = classify_role(pkg.rel_dir)
        text = (role or "").strip()
    if not text:
        return "—"
    text = re.sub(r"\s+", " ", text)
    text = text.replace("|", "｜")
    if len(text) > max_chars:
        text = text[: max_chars - 1] + "…"
    return text


def direct_dep_table_md(
    pkgs: list,
    names: set[str],
    succ: dict[str, list[str]],
    pkg_layer: dict[str, int],
) -> list[str]:
    """crate、层级、简介、直接依赖、直接被依赖。"""
    lines = [
        "### 4.3 直接依赖 / 被直接依赖（仓库内组件）",
        "",
        "下列仅统计**本仓库 137 个 crate 之间**的直接边（与 `gen_crate_docs` 的路径/workspace 解析一致）。",
        "**层级**与本文 §4.1 一致（自底向上编号，0 为仅依赖仓库外的底层）。简介优先 `Cargo.toml` 的 `description`，否则取 crate 文档摘要，否则为路径启发说明；**不超过 50 字**。",
        "列为空时记为 —。",
        "",
        "| crate | 层级 | 简介（≤50字） | 直接依赖的组件 | 直接被依赖的组件 |",
        "|-------|------|----------------|------------------|------------------|",
    ]
    for p in sorted(pkgs, key=lambda x: x.name):
        outs = sorted(succ.get(p.name, []))
        ins = sorted(x for x in p.reverse_direct if x in names)
        nm = p.name.replace("|", "\\|")
        layer = pkg_layer[p.name]
        intro = brief_intro(p)
        lines.append(
            f"| `{nm}` | {layer} | {intro} | {fmt_crate_list(outs)} | {fmt_crate_list(ins)} |"
        )
    lines.append("")
    return lines


def mermaid_full(pkgs, edges):
    by = defaultdict(list)
    for p in pkgs:
        by[classify_role(p.rel_dir)[0]].append(p)
    lines = ["```mermaid", "flowchart TB"]
    for cat in sorted(by):
        lines.append(f'    subgraph sg_{mid(cat)}["<b>{cat}</b>"]')
        lines.append("        direction TB")
        for p in sorted(by[cat], key=lambda x: x.name):
            lines.append(f'        {mid(p.name)}["{p.name}\\nv{p.version.replace(chr(34), chr(39))}"]')
        lines.append("    end")
    for a, b in sorted(edges):
        lines.append(f"    {mid(a)} --> {mid(b)}")
    lines.append(CLASS_DEF)
    for p in pkgs:
        c = classify_role(p.rel_dir)[0]
        lines.append(f"    class {mid(p.name)} {MERMAID_CLASS.get(c, 'cat_misc')}")
    lines.append("```")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--output", type=Path, default=REPO_ROOT / "docs/tgoskits-dependency.md")
    ap.add_argument("--lock", type=Path, default=REPO_ROOT / "Cargo.lock")
    args = ap.parse_args()

    print("进度: 扫描 137 个 crate 并解析依赖…", file=sys.stderr)
    pkgs = build_packages()
    succ, names = internal_graph(pkgs)
    pkg_layer, sccs = layers_from_scc(names, succ)
    edges = {(u, w) for u, ws in succ.items() for w in ws}

    ls = None
    desc_map: dict[tuple[str, str], str] = {}
    if args.lock.exists():
        print("进度: 解析 Cargo.lock 外部依赖…", file=sys.stderr)
        ls = lock_stats(args.lock, names)
        print("进度: cargo metadata（外部 crate 简介）…", file=sys.stderr)
        desc_map = cargo_package_descriptions()

    maxL = max(pkg_layer.values()) if pkg_layer else 0
    byL = defaultdict(list)
    for p in pkgs:
        byL[pkg_layer[p.name]].append(p.name)
    for L in byL:
        byL[L].sort()

    note = ""
    cyc = [c for c in sccs if len(c) > 1]
    if cyc:
        note = "\n> **说明**：存在依赖环（强连通分量），已缩点同层。\n"

    md = ["# tgoskits 组件层次依赖分析", "",
          "本文档覆盖 **137** 个 crate（与 `docs/crates/README.md` / `gen_crate_docs` 一致），按仓库内**直接**路径依赖自底向上分层。",
          "", "由 `scripts/analyze_tgoskits_deps.py` 生成。", "", "## 1. 统计概览", "",
          "| 指标 | 数值 |", "|------|------|",
          f"| 仓库内 crate | **{len(pkgs)}** |", f"| 内部有向边 | **{len(edges)}** |",
          f"| 最大层级 | **{maxL}** |", f"| SCC 数 | **{len(sccs)}** |"]
    if ls:
        md += [f"| Lock 总包块 | **{ls['lock_total']}** |",
               f"| Lock 内工作区包（与扫描交集） | **{ls['internal_in_lock']}** |",
               f"| Lock 外部依赖条目 | **{ls['external_crates']}** |"]
    md += ["", "### 1.1 分类", "", "| 分类 | 数 |", "|------|-----|"]
    cc = defaultdict(int)
    for p in pkgs:
        cc[p.category] += 1
    for c in sorted(cc):
        md.append(f"| {c} | {cc[c]} |")
    md += ["", "## 2. 依赖图（按分类子图）", "", "`A --> B` 表示 A 依赖 B。", "", mermaid_full(pkgs, edges),
           "", "## 3. 层级总览", "", note, mermaid_layers(maxL, dict(byL)), "", "## 4. 层级表", "",
           "| 层级 | 层名 | 分类 | crate | 版本 | 路径 |", "|------|------|------|-------|------|------|"]
    for p in sorted(pkgs, key=lambda x: (pkg_layer[x.name], x.category, x.name)):
        L = pkg_layer[p.name]
        ln = LAYER0 if L == 0 else "堆叠层"
        v = p.version.replace("|", "\\|")
        md.append(f"| {L} | {ln} | {p.category} | `{p.name}` | `{v}` | `{p.rel_dir}` |")
    md += ["", "### 4.2 按层紧凑", "", "| 层级 | 数 | 成员 |", "|------|-----|------|"]
    for L in range(maxL + 1):
        m = byL.get(L, [])
        md.append(f"| {L} | {len(m)} | {' '.join('`'+x+'`' for x in m)} |")
    md += direct_dep_table_md(pkgs, names, succ, pkg_layer)
    if ls and args.lock.exists():
        md += lock_section5_category_tables_md(args.lock.resolve(), names, desc_map)
    md.append("")
    args.output.write_text("\n".join(md), encoding="utf-8")
    print(f"进度: 已写入 {args.output.relative_to(REPO_ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
