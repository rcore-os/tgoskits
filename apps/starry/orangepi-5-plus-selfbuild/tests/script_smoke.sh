#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

for script in \
    benchmark.sh \
    boot_script_is_starry.sh \
    boot_starry_once.sh \
    boot_starry_once_remote.sh \
    connect_serial.sh \
    deploy_starry_boot_remote.sh \
    ensure_linux_fsck.sh \
    fetch_artifacts.sh \
    guest-selfbuild.sh \
    init.sh \
    install_source_link.sh \
    provision_rootfs.sh \
    provision_rootfs_remote.sh \
    restore_linux_boot.sh \
    run_linux_baseline.sh \
    run_linux_remote.sh \
    run_selfbuild.sh \
    set_guest_clock.sh \
    stage_starry_boot.sh \
    validate_sha256.sh; do
    bash -n "$app_dir/$script"
done

for entrypoint in \
    boot_starry_once.sh \
    connect_serial.sh \
    fetch_artifacts.sh \
    provision_rootfs.sh \
    run_linux_baseline.sh \
    run_selfbuild.sh \
    stage_starry_boot.sh; do
    "$app_dir/$entrypoint" --help >/dev/null
done

python3 - "$app_dir/serial_selfbuild.py" <<'PY'
import ast
import pathlib
import sys

ast.parse(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
PY

bash "$app_dir/tests/boot_script_identity.sh"
bash "$app_dir/tests/guest_clock.sh"
bash "$app_dir/tests/install_source_link.sh"
bash "$app_dir/tests/linux_recovery.sh"
bash "$app_dir/tests/provision_rootfs.sh"
bash "$app_dir/tests/selfbuild_contract.sh"
bash "$app_dir/tests/validate_sha256.sh"
python3 "$app_dir/tests/serial_console.py"

echo "orangepi5plus_selfbuild_script_smoke=PASS"
