# OrangePi 5 Plus 三客户机板卡用例

这个用例在 OrangePi 5 Plus 上同时启动 Linux、StarryOS 和 Zephyr，并从 Linux 客户机运行 iozone、lmbench 和 cyclictest。`build-aarch64-unknown-none-softfloat.toml` 通过 `vm_configs` 选择三份客户机配置；`board-orangepi-5-plus-linux-guest-tests.toml` 定义串口命令、超时和结果匹配规则。

## 1. 运行前提

板卡运行器只负责构建并启动 Axvisor；三份 VM 配置引用的客户机文件必须事先放入板卡宿主文件系统。运行前还需确认板卡可通过项目的 `orangepi-5-plus-linux-guest-tests` 配置访问，宿主 Linux 根分区为 `/dev/mmcblk0p2`，与板卡配置中的 `uboot_cmd` 一致。

### 1.1 客户机配置

三份配置分别指定客户机内核和设备。Linux 与 StarryOS 的 VirtIO 块设备文件由 Axvisor 从宿主 `/guest` 目录打开；Zephyr 的 `dtb_path` 是其虚拟 GIC 和 UART 描述的来源，缺失或非法时客户机无法正常启动。

| 客户机 | VM 配置 | 宿主所需文件 |
| --- | --- | --- |
| Linux | `os/axvisor/configs/vms/orangepi-5-plus/linux-virtio-blk.toml` | `/guest/linux/orangepi-5-plus`、`/guest/rootfs-aarch64-orangepi-jammy-0.img` |
| StarryOS | `os/axvisor/configs/vms/orangepi-5-plus/starry-virtio-blk.toml` | `/guest/starry/orangepi-5-plus`、`/guest/rootfs-aarch64-orangepi-jammy-1.img` |
| Zephyr | `os/axvisor/configs/vms/orangepi-5-plus/zephyr-robot.toml` | `/guest/zephyr/orangepi-5-plus`、`/guest/zephyr/orangepi-5-plus.dtb` |

这六个路径均须存在且文件非空。两份 `rootfs-aarch64-orangepi-jammy-*.img` 应为对应客户机可挂载的 ext4 根文件系统；Linux 客户机的根设备预期为 `/dev/vda`。

### 1.2 Linux 测试程序

板卡配置通过 Linux 客户机控制台运行三个程序。它们须已安装在 Linux 根文件系统镜像 `/guest/rootfs-aarch64-orangepi-jammy-0.img` 中，而非仅存在于运行 `cargo xtask` 的开发主机上。

| 程序 | Linux 客户机内路径 |
| --- | --- |
| iozone | `/guest-tests/iozone/iozone` |
| lmbench | `/guest-tests/lmbench/bin/Linux/lat_syscall` |
| cyclictest | `/guest-tests/cyclictest/cyclictest` |

测试还要求 Linux 客户机能挂载 `/proc`、`/dev/shm` 和 `/sys`，并能在 `/tmp` 写入 iozone 临时文件。`shell_check_steps` 会检查挂载、VirtIO 根设备和各程序的完成标记。

## 2. 板卡执行

从仓库根目录运行以下命令。`--test-case guest-tests` 选择本目录用例，`--board orangepi-5-plus-linux-guest-tests` 选择对应的板卡配置和串口交互步骤。

```bash
cargo xtask axvisor test board --test-case guest-tests --board orangepi-5-plus-linux-guest-tests
```

运行器依次确认三台 VM 处于 `running`、StarryOS 控制台可用、Linux 的根设备和测试程序正常，以及 Zephyr 输出。`timeout = 600` 是板卡用例的整体等待上限；不要仅凭某一条中间标记认定整项用例通过。

## 3. 验收与证据

板卡配置中的 `success_regex` 依次等待以下标记。`AXVISOR_LINUX_GUEST_TESTS_FAILED`、客户机启动错误或运行器非零退出均表示未通过；最终应同时看到 `PROJECT EXECUTION SUCCESSFUL` 和命令成功退出。

| 阶段 | 预期输出 |
| --- | --- |
| Linux 挂载准备 | `AXVISOR_LINUX_GUEST_TESTS_MOUNTS_PASSED` |
| VirtIO 根设备 | `AXVISOR_LINUX_GUEST_TESTS_VIRTIO_ROOT_PASSED` |
| iozone | `AXVISOR_LINUX_GUEST_TESTS_IOZONE_PASSED` |
| lmbench | `AXVISOR_LINUX_GUEST_TESTS_LMBENCH_PASSED` |
| cyclictest | `AXVISOR_LINUX_GUEST_TESTS_CYCLICTEST_PASSED` |
| Zephyr 收尾 | `PROJECT EXECUTION SUCCESSFUL` |

本次变更尚无三客户机板卡运行记录，因此上述内容是复现步骤与预期结果，不是已通过的执行证据。实际运行后，应在 PR 描述中记录命令、精确提交、运行环境、退出结果及日志链接；未运行前保持“未验证”状态。
