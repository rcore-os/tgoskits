#!/bin/sh
# run-nweb.sh — on-target gate for the StarryOS node-web framework carpet.
# Industrial-grade carpets for pug (template engine), express (web framework over IPv4
# loopback) and Kotlin/JS (host-precompiled module run on node). Each carpet self-checks
# (XXX_RESULT ok=N fail=0 then XXX_DONE); TEST PASSED only when every module passes.
set -u
case "$(uname -m)" in x86_64) A=x86_64;; aarch64) A=aarch64;; riscv64) A=riscv64;; loongarch64) A=loongarch64;; *) A="$(uname -m)";; esac
printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$A.path"
export PATH=/usr/bin:/bin:/sbin:/usr/sbin HOME=/root CI=1
export NODE_OPTIONS="--max-old-space-size=1024"
NODE=/usr/bin/node
D=/root/nweb

# V8 JIT probe: node runs full JIT on starry across all four arches with no kernel change;
# if a target ever lacks PROT_EXEC mmap, fall back to --jitless rather than crash.
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

echo "=== node-web: web framework carpets (pug | express | kotlin-js) ==="
run pug      PUG_DONE      carpets/PugCarpet.js
run express  EXPRESS_DONE  carpets/ExpressCarpet.js
run kotlinjs KOTLINJS_DONE carpets/KotlinJsCarpet.js
echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" -gt 0 ]; then echo "NODE_WEB_OK=$PASS/$TOTAL"; echo "TEST PASSED"; exit 0; fi
echo "TEST FAILED"; exit 1
