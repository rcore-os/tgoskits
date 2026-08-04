#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
profile_runner=${IVC_CONTROL_PROFILE_RUNNER:-$script_dir/run-orangepi-5-plus.sh}

usage() {
    cat <<EOF
Usage: $0 [smoke|formal] --result-dir <path> [options]

Options:
  --result-dir <path>      New campaign result root (required).
  --expected-commit <sha>  Required clean source commit for formal capture.
  --board <type>           Board service type (default: OrangePi-5-Plus).
  --timeout <seconds>      Timeout passed to each profile runner (default: 900).
  --dry-run                Print the frozen order without running the board.
  -h, --help               Show this help text.
EOF
}

mode=formal
if (($# > 0)) && [[ "$1" != -* ]]; then
    mode=$1
    shift
fi
result_root=
expected_commit=
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
timeout_seconds=${ORANGEPI_RUN_TIMEOUT_SECONDS:-900}
dry_run=0

while (($# > 0)); do
    case "$1" in
        --result-dir)
            result_root=${2:?--result-dir requires a value}
            shift 2
            ;;
        --expected-commit)
            expected_commit=${2:?--expected-commit requires a value}
            shift 2
            ;;
        --board)
            board_type=${2:?--board requires a value}
            shift 2
            ;;
        --timeout)
            timeout_seconds=${2:?--timeout requires a value}
            shift 2
            ;;
        --dry-run)
            dry_run=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown control campaign option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$mode" in
    smoke)
        schedule=(manual-smoke smoke)
        ;;
    formal)
        schedule=(
            manual-full full
            full manual-full
            manual-full full
            full manual-full
            manual-full full
        )
        ;;
    *)
        echo "Unsupported control campaign mode: $mode" >&2
        exit 2
        ;;
esac

if [[ -z "$result_root" ]]; then
    echo "--result-dir is required" >&2
    exit 2
fi
if [[ "$result_root" != /* ]]; then
    result_root=$workspace/$result_root
fi
if [[ -e "$result_root" ]]; then
    echo "Refusing to reuse control campaign result root: $result_root" >&2
    exit 73
fi
case "$timeout_seconds" in
    ''|*[!0-9]*)
        echo "--timeout must be a positive integer" >&2
        exit 2
        ;;
esac
if ((timeout_seconds == 0)); then
    echo "--timeout must be a positive integer" >&2
    exit 2
fi
if [[ "$mode" == formal ]]; then
    if [[ ! "$expected_commit" =~ ^[0-9a-f]{40}$ ]]; then
        echo "formal capture requires --expected-commit with a full Git SHA" >&2
        exit 2
    fi
    observed_commit=$(git -C "$workspace" rev-parse HEAD)
    if [[ "$observed_commit" != "$expected_commit" ]]; then
        echo "formal source commit differs from --expected-commit" >&2
        exit 1
    fi
    if ((dry_run == 0)) && [[ -n "$(git -C "$workspace" status --porcelain=v1)" ]]; then
        echo "formal control campaign requires a clean Git worktree" >&2
        exit 1
    fi
fi
if [[ ! -x "$profile_runner" ]]; then
    echo "Control profile runner is not executable: $profile_runner" >&2
    exit 1
fi

pair_count=$((${#schedule[@]} / 2))
for ((pair_index = 0; pair_index < pair_count; pair_index++)); do
    printf -v pair_id 'pair-%03d' "$((pair_index + 1))"
    for half_index in 0 1; do
        profile=${schedule[pair_index * 2 + half_index]}
        printf 'CONTROL_CAMPAIGN_HALF pair=%s half=%s profile=%s\n' \
            "$pair_id" "$((half_index + 1))" "$profile"
        command=(
            "$profile_runner"
            "$profile"
            --repeat 1
            --board "$board_type"
            --result-dir "$result_root/$pair_id"
            --timeout "$timeout_seconds"
            --restore-linux
            --require-clean
        )
        if ((dry_run == 0)); then
            "${command[@]}"
        else
            printf 'CONTROL_CAMPAIGN_DRY_RUN'
            printf ' %q' "${command[@]}"
            printf '\n'
        fi
    done
done

if ((dry_run == 0)); then
    echo "CONTROL_CAMPAIGN_COMPLETE mode=$mode pairs=$pair_count result_root=$result_root"
else
    echo "CONTROL_CAMPAIGN_DRY_RUN_COMPLETE mode=$mode pairs=$pair_count result_root=$result_root"
fi
