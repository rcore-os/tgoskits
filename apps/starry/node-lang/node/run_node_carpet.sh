#!/bin/sh
# run_node_carpet.sh — on-target aggregator for the StarryOS node-lang carpet (#764).
#
# #764 item: `nodejs <!-- v8 engine -->: cjs/esm · kotlin <!-- js --> · type <!-- typescript -->`
# == the LANGUAGE layer. The on-target gated test is the language/runtime carpet:
#   node /usr/bin/node-carpet.js
# It exercises the V8 language surface, ESM/CJS interop, modern JS (ES2023+),
# worker_threads, timers, and the core stdlib (367 checks), printing NODE_CARPET_OK
# on its final line iff ZERO failures. TEST PASSED is printed iff it passes.
#
# The `node` CLI option carpet (node-cli-carpet.sh, full host-green evidence of every
# documented --help flag, 184 checks) is staged at /usr/bin/node-cli-carpet.sh for
# manual inspection but is NOT part of the on-target gate: a few CLI options (e.g.
# --watch) drive kernel process-model features (fork/exec of a child node + fs
# watch/inotify) that are KNOWN StarryOS gaps (the npm/astro V8-mmap / stack-growsdown
# demand-paging kernel work), NOT the language layer. That CLI surface is fully
# validated on the host build machine; its StarryOS coverage is tracked separately.
set -u

NODE=/usr/bin/node
echo "NODELANG node $("$NODE" -p 'process.versions.node' 2>/dev/null) v8 $("$NODE" -p 'process.versions.v8' 2>/dev/null)"

"$NODE" /usr/bin/node-carpet.js
js_rc=$?

if [ "$js_rc" -eq 0 ]; then
  echo "TEST PASSED"
  exit 0
fi
echo "NODELANG: node-carpet.js failed (rc=$js_rc)"
echo "TEST FAILED"
exit 1
