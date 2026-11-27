#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage:
  assemble-autoboot.sh \
    --shell <zero-delay-BOOTX64.EFI> \
    --launcher <original-LAUNCHER.EFI> \
    --d3net <EFI/D3NET directory> \
    --output <new-or-empty-output-directory>

The --d3net input must contain the 10 D3NET EFI drivers and
check-network.nsh. load-network.nsh and startup.nsh are taken from this
script's directory so an older diagnostic package cannot override them.
The output directory is never overwritten when non-empty.
EOF
}

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

shell_src=
launcher_src=
d3net_dir=
output_dir=

while (($# > 0)); do
  case "$1" in
    --shell)
      (($# >= 2)) || fail "missing value for --shell"
      shell_src=$2
      shift 2
      ;;
    --launcher)
      (($# >= 2)) || fail "missing value for --launcher"
      launcher_src=$2
      shift 2
      ;;
    --d3net)
      (($# >= 2)) || fail "missing value for --d3net"
      d3net_dir=$2
      shift 2
      ;;
    --output)
      (($# >= 2)) || fail "missing value for --output"
      output_dir=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      fail "unknown argument: $1"
      ;;
  esac
done

[[ -n "$shell_src" ]] || { usage; fail "--shell is required"; }
[[ -n "$launcher_src" ]] || { usage; fail "--launcher is required"; }
[[ -n "$d3net_dir" ]] || { usage; fail "--d3net is required"; }
[[ -n "$output_dir" ]] || { usage; fail "--output is required"; }

[[ -f "$shell_src" ]] || fail "zero-delay Shell not found: $shell_src"
[[ -f "$launcher_src" ]] || fail "original launcher not found: $launcher_src"
[[ -d "$d3net_dir" ]] || fail "EFI/D3NET directory not found: $d3net_dir"

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
startup_src="$script_dir/startup.nsh"
load_network_src="$script_dir/load-network.nsh"

[[ -f "$startup_src" ]] || fail "startup.nsh template not found: $startup_src"
[[ -f "$load_network_src" ]] || fail "load-network.nsh template not found: $load_network_src"

required_d3net_files=(
  "DpcDxe.efi"
  "SnpDxe.efi"
  "MnpDxe.efi"
  "ArpDxe.efi"
  "Ip4Dxe.efi"
  "Udp4Dxe.efi"
  "Dhcp4Dxe.efi"
  "TcpDxe.efi"
  "RngDxe.efi"
  "Hash2DxeCrypto.efi"
  "check-network.nsh"
)

for required_file in "${required_d3net_files[@]}"; do
  [[ -f "$d3net_dir/$required_file" ]] \
    || fail "EFI/D3NET input is missing: $d3net_dir/$required_file"
done

d3net_abs=$(realpath -e -- "$d3net_dir")
output_abs=$(realpath -m -- "$output_dir")

case "$output_abs/" in
  "$d3net_abs/"*)
    fail "output directory must not be inside the EFI/D3NET input"
    ;;
esac

if [[ -e "$output_abs" ]]; then
  [[ -d "$output_abs" ]] || fail "output path exists and is not a directory: $output_abs"
  if [[ -n "$(find "$output_abs" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
    fail "output directory is not empty: $output_abs"
  fi
else
  mkdir -p -- "$output_abs"
fi

mkdir -p -- \
  "$output_abs/EFI/BOOT" \
  "$output_abs/EFI/AXLOADER" \
  "$output_abs/EFI/D3NET"

cp -a -- "$d3net_abs/." "$output_abs/EFI/D3NET/"
cp -f -- "$load_network_src" "$output_abs/EFI/D3NET/load-network.nsh"
cp -f -- "$startup_src" "$output_abs/EFI/BOOT/startup.nsh"
cp -f -- "$shell_src" "$output_abs/EFI/BOOT/BOOTX64.EFI"
cp -f -- "$launcher_src" "$output_abs/EFI/AXLOADER/LAUNCHER.EFI"
cp -f -- "$launcher_src" "$output_abs/EFI/BOOT/BOOTX64.PREVIOUS.EFI"

printf 'D3 autoboot assembly created: %s\n' "$output_abs"
printf '  EFI/BOOT/BOOTX64.EFI\n'
printf '  EFI/BOOT/BOOTX64.PREVIOUS.EFI\n'
printf '  EFI/BOOT/startup.nsh\n'
printf '  EFI/AXLOADER/LAUNCHER.EFI\n'
printf '  EFI/D3NET/{drivers, load-network.nsh, check-network.nsh, docs}\n'
