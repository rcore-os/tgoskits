# Axvisor x86_64 UEFI file 双盘用例

本用例验证 Axvisor 从文件后端提供 GPT＋ESP 启动盘，并让同一 Linux 客户机读写第二块 ext4 盘。启动资产使用 `uefiE2E` 已验证的 OVMF CODE/VARS；`assets/boot.img.gz` 是已写入本用例 GRUB 配置和 initramfs 的最终整盘镜像。`scripts/axbuild/src/axvisor/test/uefi_file.rs` 在每次运行时校验、解压并注入用例专属 rootfs，不修改 tgosimages 的发布镜像。

## 1. 启动资产

`assets/boot.img.gz` 包含 GPT 分区表、ESP、GRUB、Linux kernel 和本用例的 initramfs。`prepare_boot_image()` 校验压缩及 raw SHA-256 和 ESP 分区类型，然后解压出待注入的 `/guest/boot.img`；运行时不改写盘内文件。`assets/grub.cfg` 和 `assets/init` 保留为可审阅的源文件，单元测试检查压缩镜像内对应文件的内容与它们一致。若修改这两个源文件，需重新制作并替换压缩镜像，再更新代码中的摘要。

### 1.1 固件与分区

`assets/OVMF_CODE_4M.fd` 和 `assets/OVMF_VARS_4M.fd` 经摘要校验后，由 `prepare_configured_x86_ovmf()` 拼成内层客户机的 4 MiB 固件映像。启动镜像以 `backend = "file"`、`image_format = "raw"` 暴露，省略 `read_only` 后使用默认的可写配置；原有 `/guest/rootfs-x86_64-alpine-0.img` 以可写 ext4 文件后端暴露。

### 1.2 用例隔离

`prepare_configured_boot_rootfs()` 复制外层 Axvisor rootfs，在副本的 `/guest/boot.img` 注入启动镜像，并把该副本交给 QEMU。临时目录覆盖准备、运行和失败路径；退出后自动清理。第二块盘上的写入只证明同一次 QEMU 运行内的读写和卸载重挂载回读，不承诺跨运行保存到发布镜像。

## 2. 运行判定

VMX 与 SVM 分别通过 `cargo xtask axvisor test qemu --arch x86_64 --test-case uefi-file-vmx` 和 `--test-case uefi-file-svm` 运行。两侧共用 VM TOML、压缩盘、GRUB 配置和内层 `/init`；必须在可用的 Intel/AMD KVM 宿主上分别执行。

### 2.1 客户机检查

GRUB 增加仅存在于启动盘的内核参数。`/init` 检查该参数、GPT 磁盘头和 ESP 分区节点，以读写方式挂载 ESP，写入标记并在重挂载后回读、删除；再按 ext4 超级块识别另一台 VirtIO 盘，挂载 Alpine 文件系统、读取发行版文件、写入标记，并在卸载重挂载后回读。全部通过才输出 `AXVISOR_X86_UEFI_FILE_DUAL_DISK_PASSED`。

### 2.2 失败定位

`AXVISOR_X86_UEFI_FILE_DUAL_DISK_FAILED` 由内层 `/init` 在任一检查失败时输出，QEMU 用例的 `fail_regex` 会拒绝该结果。构建期的压缩盘、GPT/ESP、内核、固件或注入错误直接由 `xtask` 报告；它们不能被外层固件横幅或进入 shell 误判为成功。本地没有 `/dev/kvm` 时，可先使用 `--list`、`cargo xtask clippy --package axbuild` 与 `cargo xtask test --since dev` 核对发现、编排和资产处理，端到端结论仍取决于 KVM 用例。
