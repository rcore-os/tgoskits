#!/bin/sh
set -eu

if [ "$(uname -m)" != x86_64 ]; then
    echo "STARRY_OCI_RUNC_SKIPPED: runc package injection is currently validated only on x86_64"
    exit 0
fi

RUNC=/usr/bin/runc
BUNDLE=/tmp/starry-oci-runc
STATE=/tmp/starry-runc-state
CONTAINER=starry-runc-basic

test -x "$RUNC"
rm -rf "$BUNDLE" "$STATE"
mkdir -p \
    "$BUNDLE/rootfs/bin" \
    "$BUNDLE/rootfs/dev" \
    "$BUNDLE/rootfs/etc" \
    "$BUNDLE/rootfs/lib" \
    "$BUNDLE/rootfs/proc" \
    "$BUNDLE/rootfs/sys" \
    "$BUNDLE/rootfs/tmp" \
    "$STATE"

cp /bin/busybox "$BUNDLE/rootfs/bin/busybox"
ln -s busybox "$BUNDLE/rootfs/bin/sh"
cp /lib/ld-musl-x86_64.so.1 "$BUNDLE/rootfs/lib/ld-musl-x86_64.so.1"

cat > "$BUNDLE/config.json" <<'EOF'
{
  "ociVersion": "1.0.2",
  "process": {
    "terminal": false,
    "user": {
      "uid": 0,
      "gid": 0
    },
    "args": [
      "/bin/sh",
      "-c",
      "echo STARRY_OCI_RUNC_CONTAINER_OK"
    ],
    "env": [
      "PATH=/bin"
    ],
    "cwd": "/",
    "noNewPrivileges": true
  },
  "root": {
    "path": "rootfs",
    "readonly": false
  },
  "hostname": "starry-container",
  "mounts": [
    {
      "destination": "/proc",
      "type": "proc",
      "source": "proc",
      "options": [
        "nosuid",
        "noexec",
        "nodev"
      ]
    }
  ],
  "linux": {
    "cgroupsPath": "/starry-runc-basic",
    "namespaces": [
      {
        "type": "mount"
      },
      {
        "type": "pid"
      },
      {
        "type": "uts"
      },
      {
        "type": "ipc"
      },
      {
        "type": "cgroup"
      }
    ]
  }
}
EOF

mount -t tmpfs none /sys
mkdir -p /sys/fs/cgroup
if ! mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null; then
    test -r /sys/fs/cgroup/cgroup.controllers
fi
echo "STARRY_OCI_RUNC_MOUNTINFO_BEGIN"
cat /proc/self/mountinfo
echo "STARRY_OCI_RUNC_MOUNTINFO_END"

set +e
output=$(
    cd "$BUNDLE"
    "$RUNC" --root "$STATE" run "$CONTAINER" 2>&1
)
status=$?
set -e
printf '%s\n' "$output"

if [ "$status" -ne 0 ]; then
    "$RUNC" --root "$STATE" delete --force "$CONTAINER" >/dev/null 2>&1 || true
    echo "STARRY_OCI_RUNC_FAILED: runc exited with status $status"
    exit "$status"
fi

printf '%s\n' "$output" | grep -q '^STARRY_OCI_RUNC_CONTAINER_OK$'
"$RUNC" --root "$STATE" delete --force "$CONTAINER" >/dev/null 2>&1 || true
rm -rf "$BUNDLE" "$STATE"
echo "STARRY_OCI_RUNC_PASSED"
