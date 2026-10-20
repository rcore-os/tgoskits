# AxVisor 三客户机功能检查

在 Orange Pi 5 Plus 上同时运行 Linux、StarryOS 和 Zephyr，由测试者手工启动客户机、
切换控制台并执行以下检查：

| 客户机 | 检查内容 |
| --- | --- |
| StarryOS | `echo` 写入文件、`cat` 读取文件；`uname -a` 查看内核版本 |
| Linux | `echo` 写入文件、`cat` 读取文件；`cat /proc/cpuinfo` 查看 CPU 信息 |
| Zephyr | `kernel version` 查看内核版本；`kernel thread list` 查看线程；`device list` 查看设备 |

共 7 项功能检查，逐条输入命令，等待返回提示符再继续，由人观察并记录结果。

## 准备

工作站命令均在 TGOSKits 仓库根目录执行，需要 AArch64 构建环境和可用的
Orange Pi 5 Plus。将 `<板卡类型>`、`<服务地址>`、`<端口>` 替换为实际值。

### 1. 构建 StarryOS

在工作站执行：

```bash
cargo xtask starry build \
  --config apps/axvisor/triple-vm-test/configs/starry-build.toml \
  --smp 1
ls -lh target/aarch64-unknown-none-softfloat/release/starryos.bin
```

`ls` 确认生成的内核镜像。下文的 AxVisor 启动命令会将该产物随启动包传输到板卡，
无需手工复制。修改 StarryOS 源码后，先重新执行此构建命令。

### 2. 确认板端文件

从工作站连接板卡原生 Linux：

```bash
cargo xtask board connect -b <板卡类型> --server <服务地址> --port <端口>
```

在板卡原生 Linux Shell 中查看已有文件：

```sh
ls -lh /guest/linux/orangepi-5-plus \
  /guest/axvisor/zephyr/zephyr.bin /guest/axvisor/zephyr/zephyr.dtb \
  /guest/rootfs-aarch64-orangepi-jammy-0.img /guest/rootfs-aarch64-orangepi-jammy-1.img
```

上述文件均应存在且非空，直接复用原路径。
两份根盘应为可写的 ext4 文件系统，且未被原生 Linux 挂载或其他客户机使用。

Linux 内核版本为 `6.1.99-rockchip-rk3588`，支持 VirtIO-MMIO、VirtIO 块设备、
ext4 和 8250/16550 控制台。Zephyr 复用单客户机测试的 Shell 镜像及配套原始 DTB，
使用 PL011 控制台 `0x09000000`，支持本页三项 Shell 查询。

按 **Ctrl+A** 再按 **x**，等待连接命令退出、板卡释放后再启动 AxVisor。

### 3. 确认配置

| ID | 客户机 | 物理 CPU | 内存 | 根盘 |
| --- | --- | --- | --- | --- |
| 1 | Linux | CPU1，MPIDR `0x100` | 1 GiB | `/guest/rootfs-aarch64-orangepi-jammy-0.img`，客户机 `/dev/vda` |
| 2 | StarryOS | CPU3，MPIDR `0x300` | 1 GiB | `/guest/rootfs-aarch64-orangepi-jammy-1.img`，客户机 `/dev/sda` |
| 3 | Zephyr | CPU2，MPIDR `0x200` | 128 MiB | 无磁盘 |

`configs/build.toml` 的 `vm_configs` 打包三份客户机配置，无需手工传输 TOML 或执行
`vm create`。StarryOS 内核一起打包；Linux、Zephyr 镜像和两份根盘从板端上述路径读取。
`configs/board-shell.toml` 的自动检查步骤为空，进入 AxVisor 后由人启动客户机。

## 启动三个客户机

在工作站交互终端、仓库根目录执行：

```bash
cargo xtask axvisor board \
  --config apps/axvisor/triple-vm-test/configs/build.toml \
  --board-config apps/axvisor/triple-vm-test/configs/board-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

看到 `axvisor:/$` 后查看已打包的客户机配置，应包含 ID `1`、`2`、`3`：

```text
vm pool
```

此时尚未创建客户机，`vm list` 为空是正常的。先启动 Linux 并接入控制台，等待 `#` 提示符：

```text
vm start --console 1
```

按 **Ctrl+X** 再按 **h** 返回 AxVisor，启动 StarryOS，等待 `root@starry:~#`：

```text
vm start --console 2
```

再次按 **Ctrl+X**、**h** 返回 AxVisor，启动 Zephyr，观察启动信息并等待 `zephyr:~$`：

```text
vm start --console 3
```

返回 AxVisor 后查看三个客户机是否均为 `running`：

```text
vm list
```

检查期间保持三者运行。每次切换客户机前，先按 **Ctrl+X**、**h** 返回 AxVisor，
再输入对应的 `vm console <ID>`。

## 场景 1：StarryOS 客户机

在 AxVisor 管理 Shell 中进入 StarryOS，以下命令在客户机 root Shell 中执行：

```text
vm console 2
```

### 功能项 1：文件读写

```sh
echo 'hello' > /root/axvisor-test.txt
cat /root/axvisor-test.txt
```

`echo` 将 `hello` 写入测试文件，`cat` 读取文件。通过条件：读出 `hello`，返回 Shell。

清理本次测试文件，并将文件系统修改同步到根盘：

```sh
rm /root/axvisor-test.txt
sync
```

### 功能项 2：内核版本

```sh
uname -a
```

通过条件：输出内核名称、版本和体系结构，返回 Shell。记录实际输出。

## 场景 2：Linux 客户机

返回 AxVisor，在管理 Shell 中进入 Linux，以下命令在客户机 root Shell 中执行：

```text
vm console 1
```

### 准备：挂载 proc

Linux 使用 `init=/bin/sh`，查看 CPU 信息前需要挂载 proc。先查看当前挂载：

```sh
mount
```

如果 `/proc` 尚未挂载，执行以下命令；已经挂载时跳过：

```sh
mount -t proc proc /proc
```

该命令使 `/proc/cpuinfo` 等内核信息文件可用。StarryOS 已提供相应文件系统，无需此步骤。

### 功能项 1：文件读写

```sh
echo 'hello' > /root/axvisor-test.txt
cat /root/axvisor-test.txt
```

`echo` 将 `hello` 写入测试文件，`cat` 读取文件。通过条件：读出 `hello`，返回 Shell。

清理本次测试文件，并将文件系统修改同步到根盘：

```sh
rm /root/axvisor-test.txt
sync
```

### 功能项 2：CPU 信息

```sh
cat /proc/cpuinfo
```

通过条件：显示 CPU 信息，包含 `processor`，返回 Shell。记录实际输出。

## 场景 3：Zephyr 客户机

返回 AxVisor，在管理 Shell 中进入 Zephyr，以下命令在 `zephyr:~$` 后执行：

```text
vm console 3
```

### 功能项 1：内核版本

```text
kernel version
```

通过条件：输出 Zephyr 版本号，返回 Shell。

### 功能项 2：运行线程

```text
kernel thread list
```

通过条件：列出线程，包含 `shell_uart` 和 `idle`，返回 Shell。
此 Shell 镜像的线程查询命令是 `kernel thread list`。

### 功能项 3：设备列表

```text
device list
```

通过条件：列出设备，`interrupt-controller@fe600000`、`serial@9000000` 和
`serial@feb90000` 均为 `READY`，返回 Shell。

## 结束

返回 AxVisor，执行 `vm list`，确认三个客户机在检查结束时仍为 `running`。
确认 Linux、StarryOS 均已删除本次测试文件并执行 `sync` 后，停止客户机：

```text
vm stop 1
vm stop 2
vm stop 3
vm list
```

确认均为 `stopped` 后删除客户机并退出 AxVisor：

```text
vm delete 1
vm delete 2
vm delete 3
exit
```

等待 `Goodbye!`，按 **Ctrl+A** 再按 **x**，等待工作站命令退出并释放板卡。

## 记录结果

保留各条命令的实际输出，逐项记录“通过”“失败”“未支持”或“未执行”：

| 客户机 | 检查项 | 结果 |
| --- | --- | --- |
| StarryOS | `echo`、`cat` 文件读写 | 待记录 |
| StarryOS | `uname -a` 内核版本 | 待记录 |
| Linux | `echo`、`cat` 文件读写 | 待记录 |
| Linux | `cat /proc/cpuinfo` CPU 信息 | 待记录 |
| Zephyr | `kernel version` 内核版本 | 待记录 |
| Zephyr | `kernel thread list` 线程列表 | 待记录 |
| Zephyr | `device list` 设备列表 | 待记录 |

三客户机同时运行且以上 7 项均通过，才记录为本用例通过。
