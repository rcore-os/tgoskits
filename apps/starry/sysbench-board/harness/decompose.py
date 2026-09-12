#!/usr/bin/env python3
"""Decompose the StarryOS-vs-Linux sysbench gap from harness output.

Reads a Linux harness log (HL_* lines) and a StarryOS harness log (HS_* lines),
builds A55/A76 sysbench-vs-frequency reference curves from the Linux data, maps
each StarryOS core's measured throughput back to (core-type, frequency), and
reports the placement / DVFS / load-balancing decomposition — turning the raw
"~24x slower" into named, independently-actionable factors.

Usage:
    decompose.py linux-harness.out starry-harness.out
    decompose.py --selftest      # run on the 2026-07-15 measured numbers
"""
import sys
import re

PART_NAME = {"0xd05": "A55", "0xd0b": "A76"}


def kv(line):
    d = {}
    for tok in line.split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            d[k] = v
    return d


def num(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def parse_linux(text):
    cores, ref, mx, pc = {}, {}, {}, []
    for line in text.splitlines():
        f = line.split()
        if not f:
            continue
        d = kv(line)
        if f[0] == "HL_CORE":
            m = re.match(r"cpu(\d+)", f[1]) if len(f) > 1 else None
            if m:
                cores[int(m.group(1))] = d.get("part")
        elif f[0] == "HL_REF" and "cl" in d and "curkhz" in d:
            ref.setdefault(int(float(d["cl"])), []).append(
                (num(d["curkhz"]), num(d.get("ips")), num(d.get("sb"))))
        elif f[0] == "HL_PC":
            pc.append(d)
        elif f[0] == "HL_MX":
            if d.get("cpu") is None and "cpu" in f:  # "HL_MX cpu t=1 ev=.."
                pass
            key = " ".join(t for t in f[1:] if "=" not in t)  # e.g. "cpu", "thr", "mem"
            if "t" in d and "ev" in d:
                mx[f"{f[1]}_t{d['t']}"] = num(d["ev"])
            elif f[1] == "mem":
                mx["mem_line"] = line
    return cores, ref, mx, pc


def parse_starry(text):
    pc, psb, mx, memsw, pm = [], {}, {}, [], []
    for line in text.splitlines():
        f = line.split()
        if not f:
            continue
        d = kv(line)
        if f[0] == "HS_PC":
            pc.append(d)
        elif f[0] == "HS_PSB" and "c" in d and "ev" in d:
            psb[int(float(d["c"]))] = num(d["ev"])
        elif f[0] == "HS_MX" and "t" in d and "ev" in d:
            mx[f"{f[1]}_t{d['t']}"] = num(d["ev"])
        elif f[0] == "HS_MEMSW":
            memsw.append(line)
        elif f[0] == "HS_PM":
            pm.append(d)
    return pc, psb, mx, memsw, pm


def curve_k(points):
    """Proportional model ev ~= k*freq_khz (sysbench scales ~linearly with clock).
    Return (k, min_khz, max_khz, max_ev) using the sysbench (sb) column."""
    ks, khzs, evs = [], [], []
    for khz, _ips, sb in points:
        if khz and sb:
            ks.append(sb / khz)
            khzs.append(khz)
            evs.append(sb)
    if not ks:
        return None
    ks.sort()
    k = ks[len(ks) // 2]  # median slope
    return k, min(khzs), max(khzs), max(evs)


def classify(ev, curves):
    """Map a sysbench ev to (core_type, freq_khz) using the per-cluster curves.
    A candidate is valid only if the implied freq is within that cluster's range."""
    cands = []
    for name, c in curves.items():
        if not c:
            continue
        k, mn, mx, _ = c
        freq = ev / k
        if mn * 0.9 <= freq <= mx * 1.1:
            cands.append((name, freq))
    return cands


def fmt(x, nd=0):
    return "NA" if x is None else (f"{x:.{nd}f}")


def report(linux_text, starry_text):
    cores, ref, lmx, lpc = parse_linux(linux_text)
    spc, spsb, smx, smemsw, spm = parse_starry(starry_text)

    # cluster -> curve. cluster id is the cpu index we swept (0=A55, 4=A76),
    # confirmed against the MIDR map when present.
    curves = {}
    for cl, pts in ref.items():
        c = curve_k(pts)
        if not c:
            continue
        name = PART_NAME.get(cores.get(cl), f"cl{cl}")
        curves[name] = c

    out = []
    out.append("=" * 68)
    out.append("  StarryOS vs Linux — sysbench decomposition")
    out.append("=" * 68)

    out.append("\n[ Linux reference curves ]  ev ~= k * freq")
    for name, c in sorted(curves.items()):
        k, mn, mx, mxev = c
        out.append(f"  {name}: {k*1e3:.3f} ev/s per MHz, range {mn/1e3:.0f}-{mx/1e3:.0f} MHz, "
                   f"max {mxev:.0f} ev/s (@ {mx/1e3:.0f} MHz)")

    # ---- StarryOS per-core identity ----
    out.append("\n[ StarryOS per-core: measured -> (type, freq, affinity) ]")
    for d in spc:
        req = d.get("req")
        landed = d.get("landed")
        ips = num(d.get("ips"))
        part = d.get("part")
        midr_ok = d.get("midr_ok") == "1"
        pmc_ok = d.get("pmc_ok") == "1"
        mhz_pmc = num(d.get("mhz_pmc"))
        # sysbench ev pinned to this core, if available
        ev = spsb.get(int(float(req))) if req not in (None, "-1") else None
        typ = PART_NAME.get(part) if midr_ok else None
        src = "midr"
        if typ is None and req not in (None, "-1"):
            # StarryOS often doesn't emulate EL0 MIDR; the physical core at this
            # index is known from the Linux MIDR map (same board, same indices).
            typ = PART_NAME.get(cores.get(int(float(req))))
            src = "linux-map"
        cls = classify(ev, curves) if ev else []
        if typ is None and len(cls) == 1:
            typ = cls[0][0]
            src = "curve"
        freq = mhz_pmc if pmc_ok else None
        if freq is None and typ in curves and ev:
            freq = ev / curves[typ][0] / 1e3  # MHz
        aff = "ok" if (req == landed) else f"IGNORED(req {req}->{landed})"
        out.append(f"  cpu{req}: type={typ or '?'} ({src})"
                   f"  freq~={fmt(freq)}MHz{' (pmccntr)' if pmc_ok else ' (from curve)'}"
                   f"  sb={fmt(ev)}ev/s  affinity={aff}")

    # ---- decomposition on single-thread boot-core number ----
    s1 = smx.get("cpu_t1")
    out.append("\n[ Decomposition of the single-thread gap ]")
    a55 = curves.get("A55")
    a76 = curves.get("A76")
    if s1 and a55 and a76:
        # StarryOS boot core: classify s1
        cls = classify(s1, curves)
        boot_type, boot_freq = (cls[0][0], cls[0][1]) if cls else ("?", None)
        dvfs = a55[3] / s1 if boot_type == "A55" else a76[3] / s1
        placement = a76[3] / a55[3]
        out.append(f"  StarryOS 1-thread     = {s1:.0f} ev/s  "
                   f"(-> {boot_type} @ ~{boot_freq/1e3:.0f} MHz)"
                   if boot_freq else f"  StarryOS 1-thread     = {s1:.0f} ev/s")
        out.append(f"  Lever DVFS ({boot_type} -> max clock) = {dvfs:.2f}x  "
                   f"(=> {s1*dvfs:.0f} ev/s)")
        out.append(f"  Lever placement (A55 -> A76)  = {placement:.2f}x  "
                   f"(=> {s1*dvfs*placement:.0f} ev/s, ~Linux A76 1-thread)")
        # balancing from the matrix
        for t in (4, 8):
            lt = lmx.get(f"cpu_t{t}")
            if lt:
                bal = lt / a76[3]
                total = dvfs * placement * bal
                out.append(f"  Lever balancing (1 -> {t} cores) = {bal:.2f}x  "
                           f"=> total {total:.1f}x  (Linux {t}-thr {lt:.0f} / Starry {s1:.0f} "
                           f"= {lt/s1:.1f}x)")
    else:
        out.append("  (need StarryOS HS_MX cpu t=1 and Linux A55+A76 reference curves)")

    # ---- headline matrix ----
    out.append("\n[ sysbench matrix: Linux vs StarryOS ]")
    out.append(f"  {'metric':<16}{'Linux':>12}{'StarryOS':>12}")
    for t in (1, 2, 4, 8):
        out.append(f"  cpu t={t:<11}{fmt(lmx.get(f'cpu_t{t}')):>12}{fmt(smx.get(f'cpu_t{t}')):>12}")
    out.append(f"  {'thr t=4 (ev)':<16}{fmt(lmx.get('thr_t4')):>12}{fmt(smx.get('thr_t4')):>12}")

    # ---- memory ----
    out.append("\n[ Memory (the 200x anomaly) ]")
    if spm:
        for d in spm:
            out.append(f"  StarryOS membw: core={d.get('core')} landed={d.get('landed')} "
                       f"memcpy={d.get('memcpy_GBps')}GB/s read={d.get('read_GBps')}GB/s "
                       f"firsttouch={d.get('firsttouch_s')}s")
    for line in smemsw:
        out.append(f"  {line}")
    if not spm and not smemsw:
        out.append("  (no membw / memory-sweep lines found)")

    out.append("")
    return "\n".join(out)


SELFTEST_LINUX = """HARNESS_LINUX_BEGIN
HL_CORE cpu0 part=0xd05
HL_CORE cpu4 part=0xd0b
HL_REF cl=0 setkhz=408000 curkhz=408000 ips=90000000 sb=78.0
HL_REF cl=0 setkhz=600000 curkhz=600000 ips=130000000 sb=118.35
HL_REF cl=0 setkhz=816000 curkhz=816000 ips=176000000 sb=161.14
HL_REF cl=0 setkhz=1008000 curkhz=1008000 ips=220000000 sb=202.30
HL_REF cl=0 setkhz=1800000 curkhz=1800000 ips=395000000 sb=360.0
HL_REF cl=4 setkhz=408000 curkhz=408000 ips=380000000 sb=170.97
HL_REF cl=4 setkhz=600000 curkhz=600000 ips=560000000 sb=256.84
HL_REF cl=4 setkhz=816000 curkhz=816000 ips=760000000 sb=347.51
HL_REF cl=4 setkhz=2256000 curkhz=2256000 ips=2100000000 sb=976.35
HL_MX cpu t=1 ev=980.96
HL_MX cpu t=2 ev=1949.01
HL_MX cpu t=4 ev=3903.98
HL_MX cpu t=8 ev=5333.08
HL_MX thr t=4 ev=49358
HARNESS_LINUX_END
"""

SELFTEST_STARRY = """HARNESS_STARRY_BEGIN
HS_PC req=0 landed=0 cntfrq=24000000 iters=64000000 sec=0.35 ips=176500000 midr_ok=1 part=0xd05 pmc_ok=0 mhz_pmc=0.0
HS_PC req=4 landed=0 cntfrq=24000000 iters=64000000 sec=0.35 ips=176400000 midr_ok=1 part=0xd05 pmc_ok=0 mhz_pmc=0.0
HS_PSB c=0 ev=159.0
HS_PSB c=4 ev=159.4
HS_MX cpu t=1 ev=159.58
HS_MX cpu t=2 ev=160.10
HS_MX cpu t=4 ev=160.54
HS_MX thr t=4 ev=1088
HS_PM core=0 landed=0 mb=128 firsttouch_s=0.05 memcpy_GBps=0.04 read_GBps=0.05
SYSBENCH_BOARD_DONE
"""


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "--selftest":
        print(report(SELFTEST_LINUX, SELFTEST_STARRY))
        return
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(2)
    with open(sys.argv[1]) as f:
        lt = f.read()
    with open(sys.argv[2]) as f:
        st = f.read()
    print(report(lt, st))


if __name__ == "__main__":
    main()
