#!/bin/sh
# run-hdl.sh — on-target gate for the StarryOS hdl-lang carpet (#764).
#
# Staged into the rootfs by prebuild.sh and invoked by every qemu-<arch>.toml as the
# ENTIRE shell_init_cmd (`sh /usr/local/bin/run-hdl.sh`). Keeping the gate logic in a
# staged script (not inline in shell_init_cmd) is deliberate: the StarryOS app harness
# echoes the shell_init_cmd text back over the serial console, and an inline
# `echo "TEST PASSED"` would land — verbatim — in the captured stream and be matched by
# the harness `success_regex = (?m)^TEST PASSED$` as a FALSE POSITIVE (it would "pass"
# even when the real gate prints TEST FAILED). With the gate staged, the only echoed text
# is `sh /usr/local/bin/run-hdl.sh`, so the regex only ever matches this script's REAL
# stdout. (node-lang/run_node_carpet.sh uses the same pattern.)
#
# Six independent HDL-toolchain checks, each asserted byte-for-byte against its
# host-generated golden (/root/hdl-*-golden.txt). TEST PASSED is printed ONLY here, ONLY
# when all 6 pass (P==N==6).
set -u

N=0; P=0
chk() { N=$((N+1)); if [ "$1" = 1 ]; then P=$((P+1)); echo "OK   $2"; else echo "BAD  $2"; fi; }

# VLOG: static Verilator sim, stdout must EXACT-match the host verilator golden.
/usr/local/bin/hdl-tb-vlt 2>/dev/null | grep '^TB:' > /tmp/vlog.txt
if grep -q '^TB: CARPET_RESULT ALL_PASS$' /tmp/vlog.txt && cmp -s /tmp/vlog.txt /root/hdl-vlog-golden.txt; then chk 1 "VLOG verilator sim == golden"; else chk 0 "VLOG verilator"; diff /root/hdl-vlog-golden.txt /tmp/vlog.txt | head -10; fi

# IVL: same SV design as Icarus bytecode on the static vvp; must match VLOG golden.
/usr/local/bin/vvp /root/hdl-tb-ivl.vvp 2>/dev/null | grep '^TB:' > /tmp/ivl.txt
if cmp -s /tmp/ivl.txt /root/hdl-vlog-golden.txt; then chk 1 "IVL iverilog vvp == golden"; else chk 0 "IVL iverilog"; diff /root/hdl-vlog-golden.txt /tmp/ivl.txt | head -10; fi

# BSV: bsc-compiled Bluespec SystemVerilog sim.
/usr/local/bin/vvp /root/LangBSV.vvp 2>/dev/null | sed 's/[[:space:]]*$//' > /tmp/bsv.txt
if grep -q '^BSV_DONE$' /tmp/bsv.txt && cmp -s /tmp/bsv.txt /root/hdl-bsv-golden.txt; then chk 1 "BSV bluespec sim == golden"; else chk 0 "BSV bluespec"; diff /root/hdl-bsv-golden.txt /tmp/bsv.txt | head -10; fi

# BH: bsc-compiled Bluespec Classic/Haskell sim.
/usr/local/bin/vvp /root/LangBH.vvp 2>/dev/null | sed 's/[[:space:]]*$//' > /tmp/bh.txt
if grep -q '^BH_DONE$' /tmp/bh.txt && cmp -s /tmp/bh.txt /root/hdl-bh-golden.txt; then chk 1 "BH bluespec sim == golden"; else chk 0 "BH bluespec"; diff /root/hdl-bh-golden.txt /tmp/bh.txt | head -10; fi

# MAKE: static GNU Make runs a self-contained language-feature Makefile; stdout must match the host golden.
cd /root/make && /usr/local/bin/lang-make -s -f Makefile > /tmp/make.txt 2>&1; cd /root
if grep -q '^MAKE_LANG_OK$' /tmp/make.txt && cmp -s /tmp/make.txt /root/hdl-make-golden.txt; then chk 1 "MAKE gnu-make == golden"; else chk 0 "MAKE gnu-make"; diff /root/hdl-make-golden.txt /tmp/make.txt | head -10; fi

# yosys: simulate the SYNTHESIZED gate-level netlist; must match the host golden.
/usr/local/bin/vvp /root/yosys_net.vvp 2>/dev/null | grep '^SYN' > /tmp/ys.txt
if grep -q '^SYN_DONE$' /tmp/ys.txt && cmp -s /tmp/ys.txt /root/hdl-yosys-golden.txt; then chk 1 "yosys synth netlist sim == golden"; else chk 0 "yosys synth"; diff /root/hdl-yosys-golden.txt /tmp/ys.txt | head -10; fi

echo "HDL_RESULT pass=$P total=$N"
if [ "$P" = 6 ] && [ "$N" = 6 ]; then
  echo "TEST PASSED"
  exit 0
fi
echo "TEST FAILED"
exit 1
