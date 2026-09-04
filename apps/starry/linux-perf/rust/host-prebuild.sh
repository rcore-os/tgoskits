#!/usr/bin/env bash
set -euo pipefail

if [[ "${STARRY_ARCH:-}" != "aarch64" ]]; then
    echo "linux-perf host prebuild only supports aarch64" >&2
    exit 1
fi

case_dir="${STARRY_CASE_DIR:?STARRY_CASE_DIR is required}"
work_dir="${STARRY_CASE_WORK_DIR:?STARRY_CASE_WORK_DIR is required}"
overlay_dir="${STARRY_CASE_OVERLAY_DIR:?STARRY_CASE_OVERLAY_DIR is required}"
lock_file="$case_dir/packages.lock"
download_dir="$work_dir/linux-perf-apks"
runtime_root="$work_dir/linux-perf-runtime"
archive="$overlay_dir/usr/share/linux-perf/runtime.tar.gz"

for command in curl sha256sum split tar; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "missing host command: $command" >&2
        exit 1
    }
done

mkdir -p "$download_dir" "$overlay_dir/usr/share/linux-perf" "$overlay_dir/usr/bin"

while read -r sha256 filename url; do
    [[ -z "$sha256" || "$sha256" == \#* ]] && continue
    package="$download_dir/$filename"
    if [[ -f "$package" ]] && echo "$sha256  $package" | sha256sum -c - >/dev/null 2>&1; then
        continue
    fi
    curl -fL --retry 3 --connect-timeout 20 "$url" -o "$package.part"
    echo "$sha256  $package.part" | sha256sum -c - >/dev/null
    mv "$package.part" "$package"
done < "$lock_file"

# The work directory is cacheable. Recreate only this generated package tree,
# leaving the checksum-verified APK cache intact for the next run.
rm -rf "$runtime_root"
mkdir -p "$runtime_root"
while read -r sha256 filename url; do
    [[ -z "$sha256" || "$sha256" == \#* ]] && continue
    tar --warning=no-unknown-keyword -xzf "$download_dir/$filename" -C "$runtime_root"
done < "$lock_file"

tar -czf "$archive" -C "$runtime_root" .
# Board session uploads cap each file at 64 MiB. Keep the gzip stream byte-for-
# byte identical, but expose it as two deterministic transient chunks which the
# guest concatenates before extraction. This also avoids special-casing the
# package closure between QEMU and the physical board.
split -n 2 -d -a 1 "$archive" "$archive.part-"
rm -f "$archive"
cp "$case_dir/linux-perf.sh" "$overlay_dir/usr/bin/linux-perf-run"
chmod 0755 "$overlay_dir/usr/bin/linux-perf-run"
