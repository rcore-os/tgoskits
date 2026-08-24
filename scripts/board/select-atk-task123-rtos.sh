#!/usr/bin/env bash
# Resolve or RAM-boot a complete ATK-DLRK3588 Task 1/2/3 image by RTOS and
# AxVisor scheduler. Existing RT-Thread and Zephyr artifacts remain independent;
# selecting one never deletes or rewrites the other.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
zephyr_dir="${ATK_ZEPHYR_TASK123_DIR:-/home/huhu/atk-bringup/zephyr-task123-unified-20260824}"
rtthread_dir="${ATK_RTTHREAD_TASK123_DIR:-$repo_root/tmp/atk-task123-integrated-ab-20260824}"
rtos=""
scheduler=""
action="print"

main() {
    parse_arguments "$@"
    local fit
    fit="$(resolve_fit)"
    if [[ ! -f "$fit" ]]; then
        printf 'error: selected FIT does not exist: %s\n' "$fit" >&2
        exit 1
    fi
    printf 'rtos=%s scheduler=%s\n' "$rtos" "$scheduler"
    printf 'fit=%s\n' "$fit"
    printf 'sha256=%s\n' "$(sha256sum "$fit" | awk '{print $1}')"
    if [[ "$action" == "boot" ]]; then
        exec "$repo_root/scripts/board/atk-dlrk3588-ram-boot.sh" "$fit"
    fi
}

parse_arguments() {
    if [[ $# -lt 2 || $# -gt 3 ]]; then
        usage
        exit 2
    fi
    rtos="$1"
    scheduler="$2"
    if [[ ${3:-} == "--boot" ]]; then
        action="boot"
    elif [[ -n ${3:-} ]]; then
        usage
        exit 2
    fi
    case "$rtos" in
        rtthread|zephyr) ;;
        *) printf 'error: RTOS must be rtthread or zephyr\n' >&2; exit 2 ;;
    esac
    case "$scheduler" in
        rr|fp-rr) ;;
        *) printf 'error: scheduler must be rr or fp-rr\n' >&2; exit 2 ;;
    esac
}

resolve_fit() {
    case "$rtos:$scheduler" in
        zephyr:rr)
            printf '%s/axvisor-task123-zephyr-rr.fit\n' "$zephyr_dir"
            ;;
        zephyr:fp-rr)
            printf '%s/axvisor-task123-zephyr-fp-rr.fit\n' "$zephyr_dir"
            ;;
        rtthread:fp-rr)
            printf '%s/axvisor-task123-integrated-fp-rr.fit\n' "$rtthread_dir"
            ;;
        rtthread:rr)
            printf 'error: no frozen full Task 1/2/3 RT-Thread RR FIT; use fp-rr or build a separate RR arm\n' >&2
            exit 2
            ;;
    esac
}

usage() {
    printf 'usage: %s <rtthread|zephyr> <rr|fp-rr> [--boot]\n' "${BASH_SOURCE[0]##*/}" >&2
    printf 'without --boot, only resolve and hash the selected FIT\n' >&2
}

main "$@"
