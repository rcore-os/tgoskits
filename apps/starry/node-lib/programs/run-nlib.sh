#!/bin/sh
# run-nlib.sh — on-target gate for the StarryOS node-lib carpet.
# Industrial-grade carpets for the common Node.js libraries less / stylus / scss(sass) /
# @babel/core (+ TS/JSX presets) / terser / eslint and CommonJS<->ESM interop. Each carpet
# self-checks (XXX_RESULT ok=N fail=0 then XXX_DONE); TEST PASSED only when every module passes.
set -u
case "$(uname -m)" in x86_64) A=x86_64;; aarch64) A=aarch64;; riscv64) A=riscv64;; loongarch64) A=loongarch64;; *) A="$(uname -m)";; esac
printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$A.path"
export PATH=/usr/bin:/bin:/sbin:/usr/sbin HOME=/root CI=1
export NODE_OPTIONS="--max-old-space-size=1024"
NODE=/usr/bin/node
D=/root/nlib

NF=""
"$NODE" -e 'process.exit(0)' >/tmp/probe.out 2>&1
if [ $? -ne 0 ]; then echo "JIT probe failed -> --jitless"; tail -3 /tmp/probe.out; NF="--jitless"; export NODE_OPTIONS="$NODE_OPTIONS --jitless"; fi
echo "=== node $("$NODE" $NF --version 2>&1) ==="

cd "$D" || { echo "TEST FAILED"; exit 1; }
PASS=0; TOTAL=0
run(){ name="$1"; mk="$2"; js="$3"; TOTAL=$((TOTAL+1)); "$NODE" $NF "$js" >"/tmp/$name.out" 2>&1
  if grep -aq "$mk" "/tmp/$name.out" 2>/dev/null; then
    echo "  OK   $name ($(grep -aE '_RESULT ok=[0-9]+ fail=[0-9]+' "/tmp/$name.out" | head -1))"; PASS=$((PASS+1))
  else echo "  FAIL $name ($mk)"; grep -aiE 'FAIL|error|exception|throw|cannot' "/tmp/$name.out" | tail -6; fi; }

echo "=== node-lib: library carpets (less/stylus/scss | babel/terser/eslint | cjs-esm) ==="
run less    LESS_DONE    carpets/LessCarpet.js
run stylus  STYLUS_DONE  carpets/StylusCarpet.js
run scss    SCSS_DONE    carpets/ScssCarpet.js
run babel   BABEL_DONE   carpets/BabelCarpet.js
run terser  TERSER_DONE  carpets/TerserCarpet.js
run eslint  ESLINT_DONE  carpets/EslintCarpet.js
run cjsesm  CJSESM_DONE  carpets/CjsEsmCarpet.js
echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" -gt 0 ]; then echo "NODE_LIB_OK=$PASS/$TOTAL"; echo "TEST PASSED"; exit 0; fi
echo "TEST FAILED"; exit 1
