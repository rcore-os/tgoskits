#!/usr/bin/env bash
set -euo pipefail

case_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo=$(cd -- "$case_dir/../../.." && pwd -P)
board_type=
server=
port=
interactive=false
ci=false
export TRIPLE_PING_TARGET= TRIPLE_APT_PROXY= TRIPLE_LINUX_PROMPT='orangepi@orangepi5plus:~$'
timeout=600

usage() {
    cat <<'USAGE'
Usage: run.sh --board-type TYPE [--server HOST] [--port PORT]
              [--ping-target HOST] [--apt-proxy URL] [--linux-prompt PROMPT]
              [--timeout SECONDS] [--ci | --interactive]

The default and --ci modes run all three guest checks and release the board.
--interactive boots the guests and leaves the AxVisor console attached.
USAGE
}

while (($#)); do
    case "$1" in
        --board-type|--server|--port|--ping-target|--apt-proxy|--linux-prompt|--timeout)
            (($# >= 2)) || { usage >&2; exit 2; }
            case "$1" in
                --board-type) board_type=$2 ;;
                --server) server=$2 ;;
                --port) port=$2 ;;
                --ping-target) TRIPLE_PING_TARGET=$2 ;;
                --apt-proxy) TRIPLE_APT_PROXY=$2 ;;
                --linux-prompt) TRIPLE_LINUX_PROMPT=$2 ;;
                --timeout) timeout=$2 ;;
            esac
            shift 2 ;;
        --ci) ci=true; shift ;;
        --interactive) interactive=true; shift ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[[ -n $board_type && $board_type =~ ^[A-Za-z0-9._-]+$ && !( $ci == true && $interactive == true ) ]] || { usage >&2; exit 2; }
[[ -z $port || $port =~ ^[0-9]+$ && $port -ge 1 && $port -le 65535 ]] || exit 2
[[ $timeout =~ ^[0-9]+$ && $timeout -gt 0 ]] || exit 2
[[ $TRIPLE_PING_TARGET =~ ^[A-Za-z0-9.-]*$ ]] || exit 2
[[ $TRIPLE_APT_PROXY =~ ^[A-Za-z0-9:/.@_-]*$ ]] || exit 2
[[ -n $TRIPLE_LINUX_PROMPT && $TRIPLE_LINUX_PROMPT != *$'\n'* ]] || exit 2

board_config=$(mktemp)
trap 'rm -f -- "$board_config"' EXIT
printf 'board_type = "%s"\n' "$board_type" > "$board_config"
if ! $interactive; then
    sed "s/^timeout = TRIPLE_TIMEOUT$/timeout = $timeout/" \
        "$case_dir/scripts/checks.toml" >> "$board_config"
fi

command=(cargo xtask axvisor board --config apps/axvisor/triple-vm-test/configs/build.toml
         --board-config "$board_config" --board-type "$board_type")
[[ -z $server ]] || command+=(--server "$server")
[[ -z $port ]] || command+=(--port "$port")
cd "$repo"
if $interactive; then
    "${command[@]}"
    exit $?
fi
if "${command[@]}"; then
    printf 'TRIPLE_VM_TEST_PASS\n'
else
    printf 'TRIPLE_VM_TEST_FAIL\n' >&2
    exit 1
fi
