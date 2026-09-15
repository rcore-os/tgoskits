# Axvisor x86_64 UEFI 磁盘启动测试

本目录验证 Axvisor 的 x86_64 客户机通过 OVMF 从只读 VirtIO PCI block 启动。启动盘包含 GPT、FAT32 EFI System Partition、GRUB、Linux 和专用 initramfs；最终的 `AXVISOR_X86_UEFI_DISK_PASSED` 由内层 Linux 的 `/init` 输出，不依赖客户机 shell 命令注入。

## 临时资产

在 tgosimage 发布等价 raw image 前，仓库暂时提交手工构建的 `assets/uefi-guest.img.gz`。运行测试前只需校验并解压它；`axbuild` 始终接收 `.img`，不处理 gzip。

当前资产记录如下：

- raw image：256 MiB，SHA-256 `b4b41c429af81b2d3dea7214df500467eee6629ae242eedb1ac4e1b222b10f1d`；
- gzip：SHA-256 `423de35a090ae2991b3866f35261f4802e61b212299ce5e5a3aee77c38409d85`；
- GPT ESP：LBA `2048..522239`，FAT UUID `EACD-AFF8`；
- `/boot/vmlinuz`：SHA-256 `f45ef7318ef3470201cd17e17cadd62f525f053354aaaabf53ebda89bccb926d`；
- `/boot/initramfs`：SHA-256 `ed4a2fce14115adfb90744def85e37583b5f3289dbf4edcc0f623b0ee802b239`；
- `/EFI/BOOT/BOOTX64.EFI`：SHA-256 `1dc6ef0d8309e3d62dfefd77236dfcc0248b364e47cc5c8c9fc3402f38332d3e`；
- OVMF CODE：3653632 字节，SHA-256 `4be36bffc62a85538e5c2df2882da63c21a7f5683a5b439701d0823e26dcaee3`；
- OVMF VARS：540672 字节，SHA-256 `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e`。

本地准备命令与 CI 相同：

```bash
set -euo pipefail
asset_dir=test-suit/axvisor/normal/qemu-uefi-disk/assets
raw_dir=tmp/axbuild/axvisor/qemu-uefi-disk
raw_tmp=${raw_dir}/uefi-guest.img.tmp
raw_image=${raw_dir}/uefi-guest.img

(cd "${asset_dir}" && sha256sum --check uefi-guest.img.gz.sha256)
mkdir -p "${raw_dir}"
gzip -dc "${asset_dir}/uefi-guest.img.gz" > "${raw_tmp}"
raw_sha=$(awk '{print $1}' "${asset_dir}/uefi-guest.img.sha256")
printf '%s  %s\n' "${raw_sha}" "${raw_tmp}" | sha256sum --check -
mv "${raw_tmp}" "${raw_image}"
```

## 运行

在对应的 Intel VMX 或 AMD SVM KVM 主机上运行：

```bash
cargo xtask axvisor test qemu --arch x86_64 --test-group normal --test-case uefi-disk-vmx
cargo xtask axvisor test qemu --arch x86_64 --test-group normal --test-case uefi-disk-svm
```

测试先进入 Axvisor 的 `vm console 1`，随后只观察磁盘内 initramfs 的 marker。`assets/init` 和 `assets/grub.cfg` 是镜像内对应文件的明文依据。

## 更新资产

`assets/update-image.sh` 仅供维护者离线更新 fixture，CI 不调用它。脚本要求 GNU `cpio`、`gzip`、`mtools`、`fdisk` 和 `sha256sum`，会在临时 raw 副本中替换 initramfs 与 GRUB 配置，回读核对 kernel、EFI 应用和文件内容，再原子更新 gzip 及两个 checksum 文件。

```bash
test-suit/axvisor/normal/qemu-uefi-disk/assets/update-image.sh
```

更新后必须重新记录上面的 digest，并分别完成直接 QEMU + OVMF 基线、VMX 和 SVM 端到端测试。tgosimage 提供正式资产后，删除仓库内 gzip 和 CI 解压步骤，继续把拉取到的 `.img` 传给 `AXVISOR_TEST_X86_UEFI_DISK_IMAGE`。
