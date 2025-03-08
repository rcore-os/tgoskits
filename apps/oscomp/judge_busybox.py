import json
import re
import sys

# TODO: Add more commands to test here
cmds = """"""

serial_out = sys.stdin.read()
result = {}
pattern = re.compile(r"testcase (.+) (\bsuccess\b|\bfail\b)")
results = pattern.findall(serial_out)
results = {x[0].strip(): x[1] == 'success' for x in results}

for line in cmds.split('\n'):
    line = line.strip()
    if not line:
        continue
    if f"busybox {line}" not in results.keys():
        results[f"busybox {line}"] = False

results = [{
    "name": k,
    "pass": 1 if v else 0,
    "all": 1,
    "score": 1 if v else 0,
}
    for k, v in results.items()
]

for r in results:
    if r["score"] == 0:
        print(f"busybox {r['name']} failed")
        exit(255)

print(json.dumps(results))