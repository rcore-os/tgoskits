#!/bin/sh
# run-pytui.sh — on-target launcher for the Python TUI carpet (textual + casca).
#
# Sets the musl dynamic-loader search path (so the loader finds the CPython .so closure injected
# under /lib + /usr/lib), puts the pinned pure-Python site-packages (/opt/pytui) FIRST on
# PYTHONPATH so our exact textual/casca/rich versions win over anything in the base image, forces
# a deterministic non-interactive terminal environment, then hands off to python3 run_pytui.py.
# The PASS/FAIL gate (the TEST PASSED / TEST FAILED anchor) lives entirely in run_pytui.py — this
# wrapper never prints it, so the success regex cannot self-match on the launch command.
#
# The musl loader reads only /etc/ld-musl-<this-arch>.path; writing all four names is harmless and
# keeps the launcher arch-agnostic.
for a in x86_64 aarch64 riscv64 loongarch64; do
    printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$a.path"
done

export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root
export PYTHONPATH=/opt/pytui
export PYTHONDONTWRITEBYTECODE=1
export PYTHONUNBUFFERED=1
export TERM=dumb
export NO_COLOR=1
export COLUMNS=80
export LINES=24

cd /root/pytui || exit 1
exec python3 run_pytui.py
