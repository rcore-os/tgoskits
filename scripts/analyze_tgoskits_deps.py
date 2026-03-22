#!/usr/bin/env python3
"""分析 tgoskits 137 个 crate 的仓库内直接依赖，自底向上分层，生成 docs/tgoskits-dependency.md。"""
from __future__ import annotations

import argparse
import re
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


def parse_lock(path: Path):
    content = path.read_text(encoding="utf-8")
    pkgs = []
    for block in re.split(r"\n\n+", content):
        if "[[package]]" not in block:
            continue
        nm = re.search(r'^name\s*=\s*"([^"]+)"', block, re.M)
        vm = re.search(r'^version\s*=\s*"([^"]+)"', block, re.M)
        sm = re.search(r'^source\s*=\s*"([^"]+)"', block, re.M)
        if not nm or not vm:
            continue
        ds = re.search(r"^dependencies\s*=\s*\[(.*?)\]", block, re.M | re.S)
        deps = []
        if ds:
            for d in re.findall(r'"([^"]+)"', ds.group(1)):
                deps.append(d.split(" (")[0].strip().split()[0])
        pkgs.append({"name": nm.group(1), "version": vm.group(1), "source": sm.group(1) if sm else None})
    return pkgs


def lock_stats(lock_path: Path, local_names: set):
    pkgs = parse_lock(lock_path)
    ws = {p["name"] for p in pkgs if p["source"] is None}
    internal = local_names & ws
    ext = [p for p in pkgs if p["name"] not in internal and p["source"]]
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
    return {"lock_total": len(pkgs), "internal_in_lock": sum(1 for p in pkgs if p["name"] in internal),
            "external_crates": len(ext), "external_cats": dict(cats)}


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
    if args.lock.exists():
        print("进度: 解析 Cargo.lock 外部依赖…", file=sys.stderr)
        ls = lock_stats(args.lock, names)

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
    if ls and ls["external_cats"]:
        md += ["", "## 5. Lock 外部依赖（关键词粗分）", "", "| 类别 | 数 |", "|------|-----|"]
        for c in sorted(ls["external_cats"], key=lambda x: (-len(ls["external_cats"][x]), x)):
            md.append(f"| {c} | {len(ls['external_cats'][c])} |")
        md += ["", "<details><summary>列表</summary>", ""]
        for c in sorted(ls["external_cats"]):
            md.append(f"#### {c}\n")
            for it in ls["external_cats"][c]:
                md.append(f"- `{it}`")
            md.append("")
        md.append("</details>")
    md.append("")
    args.output.write_text("\n".join(md), encoding="utf-8")
    print(f"进度: 已写入 {args.output.relative_to(REPO_ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
