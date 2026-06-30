#!/usr/bin/env python3
# run_all.py — on-target gate for the StarryOS python-net framework carpet.
# Runs each framework carpet (Django / FastAPI+uvicorn / Strawberry GraphQL) as a subprocess;
# each carpet self-checks (XXX_RESULT ok=N fail=0 then XXX_DONE). TEST PASSED only when all pass.
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable
MODULES = [
    ("django", "DjangoCarpet.py", "DJANGO_DONE"),
    ("fastapi", "FastapiCarpet.py", "FASTAPI_DONE"),
    ("strawberry", "StrawberryCarpet.py", "STRAWBERRY_DONE"),
]

print("=== python %s ===" % sys.version.split()[0])
print("=== python-net: web framework carpets (django | fastapi+uvicorn | strawberry) ===")
passed = 0
for name, fn, marker in MODULES:
    r = subprocess.run([PY, os.path.join(HERE, fn)], capture_output=True, text=True)
    out = (r.stdout or "") + (r.stderr or "")
    if marker in out and r.returncode == 0:
        res = [ln for ln in out.splitlines() if "_RESULT ok=" in ln]
        print("  OK   %s (%s)" % (name, res[0].strip() if res else ""))
        passed += 1
    else:
        print("  FAIL %s (%s) rc=%s" % (name, marker, r.returncode))
        bad = [ln for ln in out.splitlines() if any(k in ln for k in ("FAIL", "Error", "Traceback", "Exception"))]
        print("\n".join(bad[-8:]))

total = len(MODULES)
print("AGGREGATE: PASS=%d TOTAL=%d" % (passed, total))
if passed == total and total > 0:
    print("PYTHON_NET_OK=%d/%d" % (passed, total))
    print("TEST PASSED")
    sys.exit(0)
print("TEST FAILED")
sys.exit(1)
