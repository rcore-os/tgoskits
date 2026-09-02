#!/usr/bin/env bash

set -euo pipefail

benchmark_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
preparer=$benchmark_dir/prepare-freestanding-c-toolchain.sh

fail() {
    echo "test_host_toolchain: $*" >&2
    exit 1
}

[[ -r "$preparer" ]] || fail "missing freestanding C toolchain preparer"
bash -n "$preparer"

temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT HUP INT TERM
fake_bin=$temporary/bin
sysroot=$temporary/sysroot
output_dir=$temporary/output
manifest=$temporary/toolchain.json
environment=$temporary/toolchain.env
mkdir -p "$fake_bin" "$sysroot/include"
for header in limits.h stdint.h string.h; do
    printf '/* test %s */\n' "$header" > "$sysroot/include/$header"
done

cat > "$fake_bin/aarch64-linux-gnu-gcc" <<'EOF'
#!/usr/bin/env sh
set -eu
case "$*" in
    -dumpmachine) printf '%s\n' aarch64-linux-gnu ;;
    '-dumpfullversion -dumpversion') printf '%s\n' 11.4.0-test ;;
    -print-multiarch) printf '%s\n' aarch64-linux-gnu ;;
    --version) printf '%s\n' 'aarch64-linux-gnu-gcc test' ;;
    *)
        output=
        while [ "$#" -gt 0 ]; do
            if [ "$1" = -o ]; then
                output=$2
                shift 2
            else
                shift
            fi
        done
        [ -n "$output" ] || {
            printf 'fake gcc: unsupported arguments and no output: %s\n' "$*" >&2
            exit 2
        }
        printf '%s\n' 'fake AArch64 object' > "$output"
        ;;
esac
EOF
cat > "$fake_bin/aarch64-linux-gnu-ar" <<'EOF'
#!/usr/bin/env sh
set -eu
if [ "${1:-}" = --version ]; then
    printf '%s\n' 'GNU ar test'
    exit 0
fi
if [ "${1:-}" = crs ] && [ "$#" -ge 3 ]; then
    printf '%s\n' 'fake AArch64 archive' > "$2"
    exit 0
fi
printf 'fake ar: unsupported arguments: %s\n' "$*" >&2
exit 2
EOF
chmod 0755 \
    "$fake_bin/aarch64-linux-gnu-gcc" \
    "$fake_bin/aarch64-linux-gnu-ar"

STARRY_RT_AARCH64_GNU_CC="$fake_bin/aarch64-linux-gnu-gcc" \
STARRY_RT_AARCH64_GNU_AR="$fake_bin/aarch64-linux-gnu-ar" \
STARRY_RT_AARCH64_GNU_SYSROOT="$sysroot" \
STARRY_RT_HOST_TOOLS_DIR="$output_dir" \
STARRY_RT_HOST_TOOLCHAIN_MANIFEST="$manifest" \
STARRY_RT_HOST_TOOLCHAIN_ENV="$environment" \
    bash "$preparer" >/dev/null

# shellcheck disable=SC1090
source "$environment"
[[ $(command -v aarch64-linux-musl-gcc) == "$output_dir/aarch64-linux-musl-gcc" ]] || \
    fail "generated compiler wrapper is not first in PATH"
[[ $(aarch64-linux-musl-gcc -print-sysroot) == "$sysroot" ]] || \
    fail "compiler wrapper did not publish the validated sysroot"
[[ $(aarch64-linux-musl-gcc -dumpmachine) == aarch64-linux-gnu ]] || \
    fail "compiler wrapper did not delegate to the validated compiler"
[[ $(aarch64-linux-musl-ar --version) == 'GNU ar test' ]] || \
    fail "archiver wrapper did not delegate to the validated archiver"

python3 - "$manifest" "$fake_bin" "$sysroot" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

manifest = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
fake_bin = Path(sys.argv[2]).resolve()
sysroot = Path(sys.argv[3]).resolve()

assert manifest["schema_version"] == 1
assert manifest["purpose"] == "StarryOS freestanding C objects and bindings"
assert manifest["target"]["machine"] == "aarch64-linux-gnu"
assert manifest["target"]["sysroot"] == str(sysroot)
assert manifest["compiler"]["path"] == str(fake_bin / "aarch64-linux-gnu-gcc")
assert manifest["compiler"]["version"] == "11.4.0-test"
assert manifest["archiver"]["path"] == str(fake_bin / "aarch64-linux-gnu-ar")
for tool in ("compiler", "archiver"):
    path = Path(manifest[tool]["path"])
    assert manifest[tool]["sha256"] == hashlib.sha256(path.read_bytes()).hexdigest()
for wrapper in ("compiler", "archiver"):
    record = manifest["wrappers"][wrapper]
    path = Path(record["path"])
    assert record["sha256"] == hashlib.sha256(path.read_bytes()).hexdigest()
PY

echo "test_host_toolchain: PASS"
