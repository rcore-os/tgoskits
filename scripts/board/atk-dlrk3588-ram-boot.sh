#!/usr/bin/env bash
# RAM-only boot of a FIT image on the ATK-DLRK3588 (RK3588) board.
#
# The vendor U-Boot on this board ships with `bootdelay=0`, so the only way to
# reach its `=>` prompt is to flood the console with Ctrl-C across the whole
# window between a reset and the first autoboot. Doing that by hand loses the
# race whenever the reset and the flood are issued as two separate steps, which
# is why this script binds them together: the flood is already running before
# the reboot is triggered, and it keeps running until the prompt is observed.
#
# The board is never written to. Only `fastboot stage` is used, which parks the
# image in the download buffer; no `flash`, `erase`, or `gpt` command is issued
# and no eMMC partition is touched. A power cycle returns the board to its
# vendor system.
#
# Vendor U-Boot quirks this flow works around:
#
#   * `fastboot boot` is parsed as an Android boot image and trips a sysmem
#     overlap check that resets the board. Use `stage` plus a manual `booti`.
#   * `iminfo`, `imxtract`, `hash`, and `sysmem` are not compiled in, so the FIT
#     is unpacked with the `fdt` command and copied with `cp.b`.
#   * The download buffer address is not exported as an environment variable.
#     It is compiled in at 0x00c00800, recovered by parsing the U-Boot binary.
#
# Usage:
#   scripts/board/atk-dlrk3588-ram-boot.sh <image.fit>
#
# Environment:
#   ATK_PORT          serial device (default /dev/ttyACM0)
#   ATK_BAUD          serial baud rate (default 1500000)
#   ATK_FASTBOOT_SN   fastboot serial number (default 8d4bd3e013e56633)
#   ATK_BREAK_WINDOW  seconds to keep flooding Ctrl-C (default 60)
#   ATK_LOG           console capture path (default a mktemp file)
#   ATK_POST_BOOT_CAPTURE seconds to keep capturing after booti (default 0)

set -euo pipefail

readonly FASTBOOT_DOWNLOAD_BUFFER=0x00c00800

port="${ATK_PORT:-/dev/ttyACM0}"
baud="${ATK_BAUD:-1500000}"
fastboot_sn="${ATK_FASTBOOT_SN:-8d4bd3e013e56633}"
break_window="${ATK_BREAK_WINDOW:-60}"
post_boot_capture="${ATK_POST_BOOT_CAPTURE:-0}"
fit_path=""
console_log=""
reader_pid=""
breaker_pid=""

main() {
    parse_arguments "$@"
    require_tools
    claim_console
    reach_uboot_prompt
    stage_fit_into_ram
    boot_fit_from_ram
    capture_post_boot
    printf 'booted %s from RAM; console capture: %s\n' "$fit_path" "$console_log"
}

capture_post_boot() {
    if [[ "$post_boot_capture" != "0" ]]; then
        printf 'capturing the booted system for %ss\n' "$post_boot_capture"
        sleep "$post_boot_capture"
    fi
}

parse_arguments() {
    if [[ $# -ne 1 ]]; then
        printf 'usage: %s <image.fit>\n' "${BASH_SOURCE[0]##*/}" >&2
        exit 2
    fi
    fit_path="$1"
    if [[ ! -f "$fit_path" ]]; then
        printf 'error: FIT image not found: %s\n' "$fit_path" >&2
        exit 2
    fi
}

require_tools() {
    local executable
    for executable in fastboot sudo; do
        if ! command -v "$executable" >/dev/null 2>&1; then
            printf 'error: required executable is unavailable: %s\n' "$executable" >&2
            exit 1
        fi
    done
    if [[ ! -e "$port" ]]; then
        printf 'error: serial port not present: %s\n' "$port" >&2
        exit 1
    fi
}

# Takes sole ownership of the console.
#
# A second reader on the same tty steals bytes from this one, which shows up
# later as truncated U-Boot replies and corrupted `fdt` output. Refuse to start
# rather than produce a confusing half-broken session.
claim_console() {
    local holders
    holders="$(sudo -n fuser "$port" 2>/dev/null || true)"
    if [[ -n "${holders// /}" ]]; then
        printf 'error: %s is already open by PID(s):%s\n' "$port" "$holders" >&2
        printf 'error: stop them first; concurrent readers drop console bytes\n' >&2
        exit 1
    fi

    console_log="${ATK_LOG:-$(mktemp -t atk-dlrk3588-console.XXXXXX.log)}"
    sudo -n stty -F "$port" "$baud" raw -echo -crtscts
    sudo -n cat "$port" >>"$console_log" &
    reader_pid=$!
    trap release_console EXIT
    printf 'console capture: %s\n' "$console_log"
}

release_console() {
    stop_break_flood
    if [[ -n "$reader_pid" ]]; then
        sudo -n kill "$reader_pid" 2>/dev/null || true
        wait "$reader_pid" 2>/dev/null || true
        reader_pid=""
    fi
}

# Walks an ordered ladder from whatever state the board is in to a `=>` prompt.
#
# Each rung is cheaper and more reliable than the one below it, and every rung
# that can trigger a reset does so with the Ctrl-C flood already running.
reach_uboot_prompt() {
    if already_at_uboot_prompt; then
        printf 'board is already at the U-Boot prompt\n'
        return
    fi

    local reset_mark reset_source
    if board_in_fastboot; then
        reset_source="fastboot"
    elif board_in_adb; then
        reset_source="vendor-adb"
    elif console_shows_axvisor_shell || detach_from_axvisor_guest_console; then
        reset_source="axvisor"
    elif console_shows_linux_shell; then
        reset_source="vendor-serial"
    else
        reset_source="manual"
    fi

    reset_mark="$(console_mark)"
    start_break_flood
    case "$reset_source" in
    fastboot)
        printf 'board is in fastboot; leaving it with the flood already running\n'
        send_console $'\003'
        ;;
    vendor-adb)
        printf 'board is in vendor Linux (adb); rebooting into U-Boot\n'
        adb reboot >/dev/null 2>&1 || true
        ;;
    axvisor)
        printf 'board is in AxVisor; rebooting into U-Boot\n'
        send_console $'reboot\r'
        ;;
    vendor-serial)
        printf 'board is in vendor Linux (serial root shell); rebooting into U-Boot\n'
        send_console $'reboot\r'
        ;;
    manual)
        printf 'cannot reach the board from the host.\n'
        printf 'press the RST button now; the Ctrl-C flood is already running.\n'
        ;;
    esac

    if ! wait_for_console "$reset_mark" '=> *$' "$break_window"; then
        stop_break_flood
        printf 'error: no U-Boot prompt within %ss; see %s\n' "$break_window" "$console_log" >&2
        exit 1
    fi
    stop_break_flood
    printf 'reached the U-Boot prompt\n'
}

already_at_uboot_prompt() {
    local mark
    mark="$(console_mark)"
    send_console $'\r'
    wait_for_console "$mark" '=> *$' 3
}

console_shows_axvisor_shell() {
    local mark
    mark="$(console_mark)"
    send_console $'\r'
    wait_for_console "$mark" 'axvisor:(/)?\$ *$' 3
}

# Returns from an attached guest console to the AxVisor shell.  Trying the
# documented escape is harmless when no guest is attached, and closes the one
# state in which the board is healthy but neither a guest shell prompt nor the
# host prompt reliably identifies who should process `reboot`.
detach_from_axvisor_guest_console() {
    local mark
    mark="$(console_mark)"
    send_console $'\030h'
    wait_for_console "$mark" 'axvisor:(/)?\$ *$' 3
}

board_in_fastboot() {
    sudo -n fastboot devices 2>/dev/null | grep -q "$fastboot_sn"
}

board_in_adb() {
    command -v adb >/dev/null 2>&1 && adb devices 2>/dev/null | grep -qE '\sdevice$'
}

console_shows_linux_shell() {
    local mark
    mark="$(console_mark)"
    send_console $'\r'
    wait_for_console "$mark" '(#|\$) *$' 3
}

# Floods Ctrl-C until told to stop.
#
# Started before any reset is triggered so there is no window in which autoboot
# can win the race.
start_break_flood() {
    (
        while :; do
            printf '\003' | sudo -n tee "$port" >/dev/null 2>&1 || true
            sleep 0.05
        done
    ) &
    breaker_pid=$!
}

stop_break_flood() {
    if [[ -n "$breaker_pid" ]]; then
        kill "$breaker_pid" 2>/dev/null || true
        wait "$breaker_pid" 2>/dev/null || true
        breaker_pid=""
    fi
}

# Parks the FIT in the U-Boot download buffer. This is the only transfer the
# script performs, and it lands in RAM only.
stage_fit_into_ram() {
    printf 'staging %s (%s bytes) into RAM\n' "$fit_path" "$(stat -c %s "$fit_path")"
    local stage_mark
    stage_mark="$(console_mark)"
    send_console $'fastboot usb 0\r'
    if ! wait_for_console "$stage_mark" 'Enter fastboot' 15; then
        printf 'error: U-Boot did not enter fastboot; see %s\n' "$console_log" >&2
        exit 1
    fi
    if ! sudo -n fastboot -s "$fastboot_sn" stage "$fit_path"; then
        printf 'error: fastboot stage failed\n' >&2
        exit 1
    fi
    stage_mark="$(console_mark)"
    send_console $'\003'
    if ! wait_for_console "$stage_mark" '=> *$' 15; then
        printf 'error: U-Boot did not return after staging; see %s\n' "$console_log" >&2
        exit 1
    fi
}

# Unpacks the staged FIT and jumps into it.
#
# Addresses and sizes are read back out of the image that is actually in RAM
# rather than assumed from the build, so a stale offset cannot send `cp.b` at
# the wrong region. The device tree is copied before the kernel: the kernel is
# the larger payload and copying it first can overrun the device tree's source
# bytes while they are still needed.
boot_fit_from_ram() {
    send_console "fdt addr $FASTBOOT_DOWNLOAD_BUFFER"$'\r'
    send_uboot_command 'fdt get addr fdtsrc /images/fdt-1 data'
    send_uboot_command 'fdt get size fdtlen /images/fdt-1 data'
    send_uboot_command 'fdt get value fdtdst /images/fdt-1 load'
    send_uboot_command 'fdt get addr kernelsrc /images/kernel-1 data'
    send_uboot_command 'fdt get size kernellen /images/kernel-1 data'
    send_uboot_command 'fdt get value kerneldst /images/kernel-1 load'
    send_uboot_command 'printenv fdtsrc fdtlen fdtdst kernelsrc kernellen kerneldst'

    send_uboot_command 'cp.b ${fdtsrc} ${fdtdst} ${fdtlen}'
    send_uboot_command 'cp.b ${kernelsrc} ${kerneldst} ${kernellen}'
    send_uboot_command 'fdt addr ${fdtdst}'
    send_uboot_command 'fdt header'

    printf 'starting the image from RAM\n'
    send_console 'booti ${kerneldst} - ${fdtdst}'$'\r'
}

# Sends one U-Boot command and waits for the prompt before sending the next.
#
# The console drops characters when commands are pushed back to back at this
# baud rate, so each command is acknowledged before the next one is written.
send_uboot_command() {
    local command="$1" mark
    mark="$(console_mark)"
    send_console "$command"$'\r'
    if ! wait_for_console "$mark" '=> *$' 10; then
        printf 'error: U-Boot did not acknowledge: %s\n' "$command" >&2
        exit 1
    fi
}

send_console() {
    printf '%s' "$1" | sudo -n tee "$port" >/dev/null
}

# Records how much console output exists right now.
#
# Callers must take this mark *before* writing to the board. The board can
# answer faster than the shell reaches the next statement, so a mark taken
# after the write can already sit past the reply.
console_mark() {
    stat -c %s "$console_log" 2>/dev/null || echo 0
}

# Waits for a regex to appear in console output produced after `mark`.
wait_for_console() {
    local mark="$1" pattern="$2" timeout="$3"
    local deadline=$((SECONDS + timeout))
    while ((SECONDS < deadline)); do
        if tail -c "+$((mark + 1))" "$console_log" 2>/dev/null |
            grep -qE "$pattern"; then
            return 0
        fi
        sleep 0.2
    done
    return 1
}

main "$@"
