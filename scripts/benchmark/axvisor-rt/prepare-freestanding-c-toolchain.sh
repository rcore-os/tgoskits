#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
gnu_cc=${STARRY_RT_AARCH64_GNU_CC:-aarch64-linux-gnu-gcc}
gnu_ar=${STARRY_RT_AARCH64_GNU_AR:-aarch64-linux-gnu-ar}
sysroot=${STARRY_RT_AARCH64_GNU_SYSROOT:-}
output_dir=${STARRY_RT_HOST_TOOLS_DIR:-$workspace/tmp/axvisor-rt/host-tools}
manifest=${STARRY_RT_HOST_TOOLCHAIN_MANIFEST:-$workspace/tmp/axvisor-rt/host-toolchain.json}
environment=${STARRY_RT_HOST_TOOLCHAIN_ENV:-$workspace/tmp/axvisor-rt/host-toolchain.env}

fail() {
    echo "AXVISOR_RT_HOST_TOOLCHAIN_FAILED: $*" >&2
    exit 1
}

resolve_command() {
    local command_name=$1
    local resolved

    resolved=$(command -v -- "$command_name") || \
        fail "required host command is missing: $command_name"
    realpath -e -- "$resolved"
}

resolve_sysroot() {
    local compiler=$1
    local configured=$2
    local multiarch
    local candidate

    if [[ -n "$configured" ]]; then
        realpath -e -- "$configured"
        return
    fi
    multiarch=$("$compiler" -print-multiarch) || \
        fail "cannot query the AArch64 compiler multiarch name"
    [[ "$multiarch" == aarch64-linux-gnu ]] || \
        fail "compiler multiarch must be aarch64-linux-gnu, got ${multiarch:-empty}"
    candidate=/usr/$multiarch
    [[ -d "$candidate/include" ]] || \
        fail "cannot derive an AArch64 header sysroot; set STARRY_RT_AARCH64_GNU_SYSROOT"
    realpath -e -- "$candidate"
}

write_compiler_wrapper() {
    local output=$1
    local compiler=$2
    local header_sysroot=$3
    local temporary

    temporary=$(mktemp "${output}.tmp.XXXXXX")
    {
        printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail'
        printf 'compiler=%q\n' "$compiler"
        printf 'header_sysroot=%q\n' "$header_sysroot"
        cat <<'EOF'
if [[ "${1:-}" == -print-sysroot ]]; then
    printf '%s\n' "$header_sysroot"
    exit 0
fi
exec "$compiler" "$@"
EOF
    } > "$temporary"
    chmod 0755 "$temporary"
    mv -f -- "$temporary" "$output"
}

write_archiver_wrapper() {
    local output=$1
    local archiver=$2
    local temporary

    temporary=$(mktemp "${output}.tmp.XXXXXX")
    {
        printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail'
        printf 'archiver=%q\n' "$archiver"
        cat <<'EOF'
exec "$archiver" "$@"
EOF
    } > "$temporary"
    chmod 0755 "$temporary"
    mv -f -- "$temporary" "$output"
}

write_environment() {
    local output=$1
    local tools_dir=$2
    local manifest_path=$3
    local temporary

    temporary=$(mktemp "${output}.tmp.XXXXXX")
    {
        printf 'export PATH=%q:' "$tools_dir"
        cat <<'EOF'
"${PATH}"
EOF
        printf 'export STARRY_RT_HOST_TOOLCHAIN_MANIFEST=%q\n' "$manifest_path"
    } > "$temporary"
    chmod 0644 "$temporary"
    mv -f -- "$temporary" "$output"
}

for command_name in bash chmod dirname git mkdir mktemp mv python3 realpath sed; do
    command -v "$command_name" >/dev/null 2>&1 || \
        fail "required host command is missing: $command_name"
done
compiler=$(resolve_command "$gnu_cc")
archiver=$(resolve_command "$gnu_ar")
machine=$("$compiler" -dumpmachine) || fail "cannot query the AArch64 compiler target"
[[ "$machine" == aarch64-linux-gnu ]] || \
    fail "compiler target must be aarch64-linux-gnu, got ${machine:-empty}"
version=$("$compiler" -dumpfullversion -dumpversion) || \
    fail "cannot query the AArch64 compiler version"
[[ -n "$version" ]] || fail "AArch64 compiler returned an empty version"
archiver_version=$("$archiver" --version | sed -n '1p') || \
    fail "cannot query the AArch64 archiver version"
[[ -n "$archiver_version" ]] || fail "AArch64 archiver returned an empty version"
sysroot=$(resolve_sysroot "$compiler" "$sysroot")
for header in limits.h stdint.h string.h; do
    [[ -r "$sysroot/include/$header" ]] || \
        fail "AArch64 header sysroot is missing include/$header: $sysroot"
done

mkdir -p -- "$output_dir" "$(dirname -- "$manifest")" "$(dirname -- "$environment")"
output_dir=$(realpath -e -- "$output_dir")
manifest=$(realpath -m -- "$manifest")
environment=$(realpath -m -- "$environment")
compiler_wrapper=$output_dir/aarch64-linux-musl-gcc
archiver_wrapper=$output_dir/aarch64-linux-musl-ar
write_compiler_wrapper "$compiler_wrapper" "$compiler" "$sysroot"
write_archiver_wrapper "$archiver_wrapper" "$archiver"

[[ $("$compiler_wrapper" -print-sysroot) == "$sysroot" ]] || \
    fail "generated compiler wrapper returned the wrong sysroot"
[[ $("$compiler_wrapper" -dumpmachine) == "$machine" ]] || \
    fail "generated compiler wrapper returned the wrong target"
"$archiver_wrapper" --version >/dev/null || \
    fail "generated archiver wrapper is unusable"

temporary_dir=$(mktemp -d)
trap 'rm -rf -- "$temporary_dir"' EXIT HUP INT TERM
cat > "$temporary_dir/smoke.c" <<'EOF'
#include <stdint.h>

uint64_t axvisor_rt_host_toolchain_smoke(uint64_t value) {
    return value + 1;
}
EOF
"$compiler_wrapper" \
    -std=gnu99 -ffreestanding -fno-builtin -mgeneral-regs-only \
    -c "$temporary_dir/smoke.c" -o "$temporary_dir/smoke.o"
"$archiver_wrapper" crs "$temporary_dir/libsmoke.a" "$temporary_dir/smoke.o"
[[ -s "$temporary_dir/libsmoke.a" ]] || fail "freestanding compiler smoke archive is empty"

export AXVISOR_RT_TOOLCHAIN_COMPILER=$compiler
export AXVISOR_RT_TOOLCHAIN_COMPILER_VERSION=$version
export AXVISOR_RT_TOOLCHAIN_ARCHIVER=$archiver
export AXVISOR_RT_TOOLCHAIN_ARCHIVER_VERSION=$archiver_version
export AXVISOR_RT_TOOLCHAIN_MACHINE=$machine
export AXVISOR_RT_TOOLCHAIN_SYSROOT=$sysroot
export AXVISOR_RT_TOOLCHAIN_COMPILER_WRAPPER=$compiler_wrapper
export AXVISOR_RT_TOOLCHAIN_ARCHIVER_WRAPPER=$archiver_wrapper
manifest_temporary=$(mktemp "${manifest}.tmp.XXXXXX")
python3 - "$manifest_temporary" <<'PY'
import hashlib
import json
import os
import sys
from pathlib import Path


def record(path_value: str, *, version: str | None = None) -> dict[str, object]:
    path = Path(path_value).resolve()
    value: dict[str, object] = {
        "path": str(path),
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "size_bytes": path.stat().st_size,
    }
    if version is not None:
        value["version"] = version
    return value


document = {
    "schema_version": 1,
    "purpose": "StarryOS freestanding C objects and bindings",
    "target": {
        "machine": os.environ["AXVISOR_RT_TOOLCHAIN_MACHINE"],
        "sysroot": os.environ["AXVISOR_RT_TOOLCHAIN_SYSROOT"],
    },
    "compiler": record(
        os.environ["AXVISOR_RT_TOOLCHAIN_COMPILER"],
        version=os.environ["AXVISOR_RT_TOOLCHAIN_COMPILER_VERSION"],
    ),
    "archiver": record(
        os.environ["AXVISOR_RT_TOOLCHAIN_ARCHIVER"],
        version=os.environ["AXVISOR_RT_TOOLCHAIN_ARCHIVER_VERSION"],
    ),
    "wrappers": {
        "compiler": record(os.environ["AXVISOR_RT_TOOLCHAIN_COMPILER_WRAPPER"]),
        "archiver": record(os.environ["AXVISOR_RT_TOOLCHAIN_ARCHIVER_WRAPPER"]),
    },
}
Path(sys.argv[1]).write_text(
    json.dumps(document, indent=2, sort_keys=True, allow_nan=False) + "\n",
    encoding="utf-8",
    newline="\n",
)
PY
mv -f -- "$manifest_temporary" "$manifest"
write_environment "$environment" "$output_dir" "$manifest"

echo "AXVISOR_RT_HOST_TOOLCHAIN_READY manifest=$manifest compiler=$compiler version=$version target=$machine sysroot=$sysroot"
