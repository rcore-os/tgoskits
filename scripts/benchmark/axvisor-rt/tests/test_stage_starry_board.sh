#!/usr/bin/env bash

set -euo pipefail

benchmark_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
stage_runner=$benchmark_dir/stage-starry-board.sh
temporary_root=$(mktemp -d)

cleanup() {
    rm -rf -- "$temporary_root"
}
trap cleanup EXIT HUP INT TERM

fail() {
    echo "test_stage_starry_board: $*" >&2
    exit 1
}

fake_bin=$temporary_root/bin
mkdir -p "$fake_bin"

cat >"$fake_bin/cargo" <<'FAKE_CARGO'
#!/usr/bin/env bash
set -euo pipefail
[[ -z ${ORANGEPI_SUDO_PASSWORD+x} ]]
[[ "$*" == "xtask board connect -b OrangePi-5-Plus" ]]
printf '%s\n' 'Allocated board session:' '  board_id: orangepi-5-plus-1'
trap 'exit 0' TERM INT
while true; do
    sleep 1
done
FAKE_CARGO

cat >"$fake_bin/rsync" <<'FAKE_RSYNC'
#!/usr/bin/env bash
set -euo pipefail
[[ -z ${ORANGEPI_SUDO_PASSWORD+x} ]]
printf 'rsync' >>"$FAKE_RSYNC_LOG"
printf ' %q' "$@" >>"$FAKE_RSYNC_LOG"
printf '\n' >>"$FAKE_RSYNC_LOG"
FAKE_RSYNC

cat >"$fake_bin/ssh" <<'FAKE_SSH'
#!/usr/bin/env bash
set -euo pipefail
[[ -z ${ORANGEPI_SUDO_PASSWORD+x} ]] || {
    echo 'sudo password leaked into the ssh environment' >&2
    exit 79
}

if [[ " $* " == *' mkdir -p -- '* ]]; then
    printf '%s\n' mkdir >>"$FAKE_SSH_LOG"
    exit 0
fi

arguments=("$@")
argument_count=${#arguments[@]}
if ((argument_count >= 7)); then
    cleanup_offset=$((argument_count - 7))
    if [[ "${arguments[cleanup_offset]}" == sudo ]]; then
        expected_cleanup=(sudo -S rm -f -- /home/rt /home/rt.host.log)
        for index in "${!expected_cleanup[@]}"; do
            [[ "${arguments[cleanup_offset + index]}" == "${expected_cleanup[index]}" ]] || {
                echo 'remote staging cleanup escaped its exact sudo/path contract' >&2
                exit 80
            }
        done
        sudo_password=$(cat)
        [[ "$sudo_password" == "$FAKE_EXPECTED_SUDO_PASSWORD" ]] || {
            echo 'remote staging cleanup did not receive the configured sudo password' >&2
            exit 81
        }
        [[ "$*" != *"$FAKE_EXPECTED_SUDO_PASSWORD"* ]] || {
            echo 'sudo password leaked into the ssh argument vector' >&2
            exit 82
        }
        printf '%s\n' cleanup >>"$FAKE_SSH_LOG"
        exit 0
    fi
fi

remote_script=$(cat)
[[ "$remote_script" != *'sudo -n rm -f'* ]] || {
    echo 'remote staging transaction retained the invalid passwordless cleanup' >&2
    exit 83
}
[[ "$remote_script" != *"$FAKE_EXPECTED_SUDO_PASSWORD"* ]] || {
    echo 'sudo password leaked into the remote staging script' >&2
    exit 84
}
[[ "$remote_script" == set\ -eu* ]] || {
    echo 'remote staging transaction script is missing' >&2
    exit 85
}

printf '%s\n' transaction >>"$FAKE_SSH_LOG"
printf '%s\n' \
    'starryos.bin: OK' \
    'starry-orangepi-5-plus.dtb: OK' \
    'starry-rt-capture-rootfs.img: OK' \
    '/dev/mmcblk1p2 ext4 rw,noatime,errors=remount-ro,commit=600' \
    'AXVISOR_RT_BOARD_IDENTITY board_id=bf61f4d4a1d994ad hostname=orangepi5plus cpu_temp_milli_c=39000' \
    'AXVISOR_RT_BOARD_STAGE_PASS'
FAKE_SSH

chmod +x "$fake_bin/cargo" "$fake_bin/rsync" "$fake_bin/ssh"

kernel=$temporary_root/starryos.bin
dtb=$temporary_root/starry-orangepi-5-plus.dtb
rootfs=$temporary_root/starry-rt-capture-rootfs.img
identity=$temporary_root/orangepi_automation
printf '%s\n' kernel >"$kernel"
printf '%s\n' dtb >"$dtb"
printf '%s\n' rootfs >"$rootfs"
printf '%s\n' identity >"$identity"

export FAKE_EXPECTED_SUDO_PASSWORD='board-test-password'
export FAKE_RSYNC_LOG=$temporary_root/rsync.log
export FAKE_SSH_LOG=$temporary_root/ssh.log
export ORANGEPI_SSH_IDENTITY=$identity
export ORANGEPI_SSH_TARGET=orangepi@test.invalid
export ORANGEPI_SUDO_PASSWORD=$FAKE_EXPECTED_SUDO_PASSWORD

output=$(
    PATH="$fake_bin:$PATH" bash "$stage_runner" \
        --kernel "$kernel" \
        --dtb "$dtb" \
        --rootfs "$rootfs" \
        --guest-dir /home/orangepi/axvisor-guest-test
)

[[ "$output" == *'AXVISOR_RT_BOARD_STAGE_PASS'* ]] || \
    fail "stage completion proof is missing: $output"
[[ "$output" == *'AXVISOR_RT_BOARD_STAGE_COMPLETE '* ]] || \
    fail "stage final marker is missing: $output"
[[ "$output" != *"$FAKE_EXPECTED_SUDO_PASSWORD"* ]] || \
    fail "sudo password leaked into stage output"
[[ $(wc -l <"$FAKE_RSYNC_LOG") -eq 4 ]] || \
    fail "stage did not transfer the three artifacts and manifest"
mapfile -t ssh_calls <"$FAKE_SSH_LOG"
[[ "${ssh_calls[*]}" == 'mkdir cleanup transaction' ]] || \
    fail "stage SSH sequence differs from mkdir/cleanup/transaction: ${ssh_calls[*]}"
if grep -Fq "$FAKE_EXPECTED_SUDO_PASSWORD" "$FAKE_SSH_LOG"; then
    fail "sudo password leaked into captured SSH arguments"
fi

echo "test_stage_starry_board: PASS"
