#!/usr/bin/env bash
set -euo pipefail

asset_dir=$(cd -- "$(dirname -- "$0")" && pwd)
for tool in gzip cpio mcopy mtype sha256sum fdisk; do
  command -v "${tool}" >/dev/null || {
    echo "missing required tool: ${tool}" >&2
    exit 1
  }
done

work_dir=$(mktemp -d)
trap 'rm -rf "${work_dir}"' EXIT
raw_image=${work_dir}/uefi-guest.img
esp_offset=1048576
expected_size=268435456
expected_kernel_sha=f45ef7318ef3470201cd17e17cadd62f525f053354aaaabf53ebda89bccb926d
expected_bootx64_sha=1dc6ef0d8309e3d62dfefd77236dfcc0248b364e47cc5c8c9fc3402f38332d3e

(cd "${asset_dir}" && sha256sum --check uefi-guest.img.gz.sha256)
gzip -dc "${asset_dir}/uefi-guest.img.gz" > "${raw_image}"
[[ $(stat -c %s "${raw_image}") = "${expected_size}" ]]

partition=$(
  fdisk -l "${raw_image}" |
    awk -v image="${raw_image}1" '$1 == image { print $2 " " $3 " " $4 }'
)
[[ ${partition} = "2048 522239 520192" ]] || {
  echo "unexpected ESP layout: ${partition:-missing}" >&2
  exit 1
}

mcopy -i "${raw_image}@@${esp_offset}" ::/boot/initramfs "${work_dir}/initramfs.old.gz"
mtype -i "${raw_image}@@${esp_offset}" ::/boot/grub/grub.cfg > "${work_dir}/grub.old.cfg"
mkdir "${work_dir}/initramfs"
(
  cd "${work_dir}/initramfs"
  gzip -dc "${work_dir}/initramfs.old.gz" | cpio -id --no-absolute-filenames
  install -m 0755 "${asset_dir}/init" init
  find . -exec touch -h -d '@0' {} +
  find . -mindepth 1 -print0 | LC_ALL=C sort -z |
    cpio --null -o -H newc --reproducible --owner=0:0 |
    gzip -n > "${work_dir}/initramfs.new.gz"
)

install -m 0644 "${asset_dir}/grub.cfg" "${work_dir}/grub.cfg"
touch -d '1980-01-01 UTC' "${work_dir}/initramfs.new.gz" "${work_dir}/grub.cfg"
if ! cmp -s "${work_dir}/initramfs.old.gz" "${work_dir}/initramfs.new.gz"; then
  mcopy -o -m -i "${raw_image}@@${esp_offset}" \
    "${work_dir}/initramfs.new.gz" ::/boot/initramfs
fi
if ! cmp -s "${work_dir}/grub.old.cfg" "${work_dir}/grub.cfg"; then
  mcopy -o -m -i "${raw_image}@@${esp_offset}" \
    "${work_dir}/grub.cfg" ::/boot/grub/grub.cfg
fi

mcopy -i "${raw_image}@@${esp_offset}" ::/boot/initramfs "${work_dir}/initramfs.verify.gz"
cmp "${work_dir}/initramfs.new.gz" "${work_dir}/initramfs.verify.gz"
mtype -i "${raw_image}@@${esp_offset}" ::/boot/grub/grub.cfg > "${work_dir}/grub.verify.cfg"
cmp "${asset_dir}/grub.cfg" "${work_dir}/grub.verify.cfg"
mcopy -i "${raw_image}@@${esp_offset}" ::/boot/vmlinuz "${work_dir}/vmlinuz"
mcopy -i "${raw_image}@@${esp_offset}" ::/EFI/BOOT/BOOTX64.EFI "${work_dir}/BOOTX64.EFI"
printf '%s  %s\n' "${expected_kernel_sha}" "${work_dir}/vmlinuz" | sha256sum --check -
printf '%s  %s\n' "${expected_bootx64_sha}" "${work_dir}/BOOTX64.EFI" | sha256sum --check -

gzip -n -c "${raw_image}" > "${work_dir}/uefi-guest.img.gz"
raw_sha=$(sha256sum "${raw_image}" | awk '{print $1}')
gzip_sha=$(sha256sum "${work_dir}/uefi-guest.img.gz" | awk '{print $1}')
printf '%s  uefi-guest.img\n' "${raw_sha}" > "${work_dir}/uefi-guest.img.sha256"
printf '%s  uefi-guest.img.gz\n' "${gzip_sha}" > "${work_dir}/uefi-guest.img.gz.sha256"

mv "${work_dir}/uefi-guest.img.gz" "${asset_dir}/uefi-guest.img.gz"
mv "${work_dir}/uefi-guest.img.sha256" "${asset_dir}/uefi-guest.img.sha256"
mv "${work_dir}/uefi-guest.img.gz.sha256" "${asset_dir}/uefi-guest.img.gz.sha256"

echo "raw SHA-256: ${raw_sha}"
echo "gzip SHA-256: ${gzip_sha}"
