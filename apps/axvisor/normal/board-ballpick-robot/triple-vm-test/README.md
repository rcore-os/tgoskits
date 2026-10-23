# AxVisor 三客户机功能与性能检查

在 Orange Pi 5 Plus 上同时运行 Linux、StarryOS 和 Zephyr，由测试者手工启动客户机、
切换控制台并执行以下检查：

| 客户机 | 检查内容 |
| --- | --- |
| StarryOS | `echo` 写入文件、`cat` 读取文件；`uname -a` 查看内核版本 |
| Linux | `echo` 写入文件、`cat` 读取文件；`cat /proc/cpuinfo` 查看 CPU 信息 |
| Zephyr | `kernel version` 查看内核版本；`kernel thread list` 查看线程；`device list` 查看设备 |

功能场景执行上述 7 项检查。性能场景让 StarryOS 执行 cyclictest、Linux 执行 IOZone，
并在二者运行时执行 Zephyr 的 ARM 定时器中断时延测量。
逐条输入命令，由人观察并记录结果；切换控制台不会停止已经启动的负载。

两种场景共用 `configs/build.toml`、三份客户机配置和原有 Zephyr Shell 固件，无需重编译 Zephyr。
同一次启动即可执行功能和性能检查。StarryOS、Linux 的功能命令可在各自负载运行时执行；
Zephyr 的三个普通查询命令在本轮中断测量结束、返回 Shell 后执行。

## 准备

工作站命令均在 TGOSKits 仓库根目录执行，需要 AArch64 构建环境和可用的
Orange Pi 5 Plus。将 `<板卡类型>`、`<服务地址>`、`<端口>` 替换为实际值。

### 1. 标准镜像

板卡根文件系统使用标准镜像 `orangepi-5-plus.img.tar.xz`。镜像中已有客户机内核、
Zephyr DTB、两份 ext4 根盘，以及根盘内的 cyclictest 和 IOZone，无需另外编译或传输这些文件。
配置直接读取以下路径，不复制、不重命名：

| 资源 | 板端路径 |
| --- | --- |
| Linux 6.1.99-rockchip-rk3588 | `/guest/linux/orangepi-5-plus` |
| StarryOS | `/guest/starry/orangepi-5-plus` |
| Zephyr 4.4.99 Shell | `/guest/zephyr/orangepi-5-plus` |
| Zephyr DTB | `/guest/zephyr/orangepi-5-plus.dtb` |
| Linux 根盘 | `/guest/rootfs-aarch64-orangepi-jammy-0.img` |
| StarryOS 根盘 | `/guest/rootfs-aarch64-orangepi-jammy-1.img` |

两份根盘不能同时被原生 Linux 挂载或其他客户机使用。Linux 使用 VirtIO 块设备和
8250/16550 控制台。Zephyr 使用 PL011 控制台，提供三个查询命令和 `irq latency`。

### 2. 客户机资源

| ID | 客户机 | 物理 CPU | 内存 | 根盘 |
| --- | --- | --- | --- | --- |
| 1 | Linux | CPU1，MPIDR `0x100` | 1 GiB | `/guest/rootfs-aarch64-orangepi-jammy-0.img`，客户机 `/dev/vda` |
| 2 | StarryOS | CPU3，MPIDR `0x300` | 1 GiB | `/guest/rootfs-aarch64-orangepi-jammy-1.img`，客户机 `/dev/sda` |
| 3 | Zephyr | CPU2，MPIDR `0x200` | 128 MiB | 无磁盘 |

`configs/build.toml` 的 `vm_configs` 打包三份客户机配置，无需手工传输 TOML 或执行
`vm create`。三个内核、Zephyr DTB 和两份根盘均从标准镜像的上述路径读取。
Zephyr 的 BIN、DTB 均使用板端原文件，保留 PL011 控制台和无物理输出的虚拟 UART6。
`configs/zephyr.toml` 不声明 `console0`，由原 DTB 提供 PL011 的地址 `0x09000000`、
中断号 33 和节点身份。当前 AxVisor 中显式 `console0` 优先于客户机 DTB，不能仅指定型号后依赖 DTB 补全资源。
`configs/uboot-shell.toml` 的自动检查步骤为空，进入 AxVisor 后由人启动客户机。

## 启动三个客户机

更新配置后，先按本页“结束”停止客户机并退出当前 AxVisor 会话，再重新执行下面的启动命令，
将新配置重新打包。旧会话中仅执行 `vm start` 不会读取工作站上修改后的 TOML。

性能场景先在工作站开始记录串口输出，以便保存 Zephyr 的时延报告：

```bash
mkdir -p target/axvisor/triple-vm-test
script -q -f target/axvisor/triple-vm-test/serial.log
```

`script` 打开一个记录终端输入输出的 Shell，后续命令仍由人执行；已有同名日志时先另行保存。
仅做功能检查可以跳过录制。随后在工作站交互终端、仓库根目录执行统一的启动命令：

```bash
cargo xtask axvisor board \
  --config apps/axvisor/normal/board-ballpick-robot/triple-vm-test/configs/build.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/triple-vm-test/configs/uboot-shell.toml \
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

仅做功能检查时按场景 1、2、3 依次执行；做性能检查时按场景 4 启动负载，
StarryOS、Linux 的功能命令可在各自负载运行期间执行，Zephyr 的功能查询在其本轮测量结束后执行。

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

首次执行 `mount` 若报告无法读取 mtab，也先执行上述 proc 挂载，再查看挂载列表。
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

返回 AxVisor，在管理 Shell 中进入 Zephyr，以下命令在 `zephyr:~$` 后执行。
若正在运行 `irq latency`，先等待本轮测量结束，或按 Ctrl+C 取消，返回普通 Shell 后再查询：

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
本固件的线程查询命令是 `kernel thread list`。

### 功能项 3：设备列表

```text
device list
```

通过条件：列出设备并返回 Shell，`interrupt-controller@fe600000`、
`serial@9000000`、`serial@feb90000` 均为 `READY`。

## 场景 4：三客户机并发性能检查

沿用前面的统一启动配置和原 Zephyr 固件，依次在 StarryOS 启动 cyclictest、在 Linux 启动 IOZone，
再进入 Zephyr 执行中断时延测量。下面各节标明命令的执行位置，切换控制台后再继续操作。

StarryOS 根盘应包含 `/guest-tests/cyclictest/cyclictest`，
Linux 根盘应包含 `/guest-tests/iozone/iozone`。
日志和报告使用固定文件名，重复运行会覆盖；需要保留时请提前备份。

### 1. StarryOS 启动 cyclictest

在 AxVisor 管理 Shell（`axvisor:/$`）中进入 StarryOS：

```text
vm console 2
```

看到 `root@starry:~#` 后执行。StarryOS 已挂载 proc 和 sysfs，无需额外挂载：

```sh
/guest-tests/cyclictest/cyclictest \
  --default-system -S -p 90 -i 1000 -d 0 -D 600 -u \
  2>&1 | tee /tmp/latency-ssh.log &
```

本命令使用 rt-tests 2.11 的 cyclictest。`-S` 在单 vCPU 客户机上创建一个测试线程，
`-p 90` 设置 FIFO 优先级，`-i 1000 -d 0` 设置 1000 微秒唤醒间隔，`-D 600` 最多运行 600 秒。
`-u` 使输出及时刷新，`tee` 同时显示并保存日志，末尾的 `&` 使测试在后台继续运行。
未使用 `-m`，因此结果是在未锁定内存条件下测得的唤醒时延。

观察输出中的采样计数 `C` 持续增长，以及 `Min`、`Avg`、`Max` 时延，单位为微秒。
切换到 Linux 后，cyclictest 继续运行。

### 2. Linux 启动 IOZone

按 Ctrl+X、h 返回 AxVisor，在 `axvisor:/$` 中进入 Linux：

```text
vm console 1
```

看到 Linux 的 `#` 提示符后，先按场景 2 的准备步骤确认 proc 已挂载；已完成时跳过。
检查 Linux 根盘的空闲空间：

```sh
df -Pk /root
```

512 MiB 工作文件需要 `Available` 至少为 `589824` KiB，包含 64 MiB 余量。
空间不足时先清理本次测试遗留的工作文件；仍不足则扩容根盘，达到门槛后再继续，不缩小测试文件。然后在 Linux 中启动测试：

```sh
/guest-tests/iozone/iozone \
  -a -n 512m -g 512m -i 0 -i 1 -i 2 \
  -f /root/iozone.tmp -Rb /root/test.xls \
  2>&1 | tee /root/iozone-live.log &
```

`-a` 自动选择记录大小，`-n 512m -g 512m` 将文件大小固定为 512 MiB。
`-i 0` 测顺序写/重写，`-i 1` 测读/重读，`-i 2` 测随机读写。
`-f` 指定工作文件，`-R -b` 将报告保存到 `/root/test.xls`；`tee` 同时显示并保存文本输出。

看到 IOZone 开始输出后，在测试仍运行时进入下一节，使三个负载重叠。

### 3. Zephyr 执行中断时延测量

按 Ctrl+X、h 返回 AxVisor，在 `axvisor:/$` 中进入 Zephyr：

```text
vm console 3
```

看到 `zephyr:~$` 后，执行原固件的定时器中断测量命令：

```text
irq latency 30000
```

`30000` 是本轮最长等待时间，单位为毫秒。看到 `IRQ_LATENCY_BEGIN source=arch_timer` 后，
等待采样结束。目标为 10000 个样本，采满后可提前结束，并打印 `IRQ_LATENCY`、
`IRQ_INTERVAL_DEVIATION` 的 Min/Avg/Max（cycles、ns），以及 `IRQ_TIMER_STATS` 和
`IRQ_LATENCY_PASSED`。

测量期间输入 `status` 可查看状态，Ctrl+C 可取消；按 Ctrl+X、h 切换到其他客户机时测量继续运行。
需要多轮时，返回 `zephyr:~$` 后由人再次输入 `irq latency 30000`。
原固件在每轮结束时打印统计，测量期间不能执行普通 Shell 查询。

StarryOS、Linux 的功能检查可在各自负载运行时执行；Zephyr 的版本、线程和设备查询待本轮测量结束后执行。
记录各负载的实际重叠情况，Zephyr 每轮开始时应有 cyclictest 和 IOZone 同时运行。

### 4. Linux 收取 IOZone 结果

按 Ctrl+X、h 返回 AxVisor，执行 `vm console 1` 进入 Linux。等待 IOZone 完成及后台输出写完：

```sh
wait
tail -n 20 /root/iozone-live.log
ls -lh /root/test.xls
```

检查末尾出现 `iozone test complete`，数据行文件大小为 `524288` KiB，实际测试的顺序及随机读写有数值，且 `test.xls` 非空。
IOZone 自动模式可能跳过部分记录大小；`Error writing block`、`interrupted` 或只有零值的报告表示本轮未完成。
这些结果包含客户机页缓存和文件后端开销。保存日志和报告后，删除本次工作文件：

```sh
rm -f /root/iozone.tmp
sync
```

### 5. StarryOS 保存 cyclictest 结果

按 Ctrl+X、h 返回 AxVisor，执行 `vm console 2` 进入 StarryOS。
等待 cyclictest 达到 `-D 600` 指定的时长并自行结束，再保存结果。
需要提前结束时，先使用 Shell 内置命令查看本终端启动的后台任务：

```sh
jobs -l
```

将 `<PID>` 替换为 cyclictest 进程的编号，向它发送 SIGINT；不要结束同一管道中的 `tee`：

本页管道的前一个进程是 cyclictest，后一个是 tee。`jobs -l` 没有显示程序名时，
按这个顺序识别两个进程号。

```sh
kill -INT <PID>
```

等待后台输出写完，查看最终统计并复制日志：

```sh
wait
tail -n 10 /tmp/latency-ssh.log
cp /tmp/latency-ssh.log /root/latency-ssh.log
sync
```

记录采样数、Min/Avg/Max 及是否提前停止。StarryOS 的 `/tmp` 在 VM 停止后消失，
因此停止 VM 前需完成上述复制；持久化日志为客户机内的 `/root/latency-ssh.log`。

### 6. 结果文件与问题处理

保留本次测量输出，再按本页“结束”停止三个客户机。各文件的读取位置如下：

| 执行位置 | 文件 | 内容 |
| --- | --- | --- |
| StarryOS 客户机 | `/root/latency-ssh.log` | cyclictest 日志 |
| Linux 客户机 | `/root/iozone-live.log` | IOZone 文本输出 |
| Linux 客户机 | `/root/test.xls` | IOZone 报告 |
| 工作站 | `target/axvisor/triple-vm-test/serial.log` | 串口录制，含 Zephyr 时延输出 |

工具缺失时检查是否使用标准镜像及正确根盘。cyclictest 报不支持选项或 NUMA 错误时，
执行 `/guest-tests/cyclictest/cyclictest --help` 核对版本和参数支持，再使用本页的测试参数。

需要提前取消 IOZone 时，在 Linux 中执行 `ps -C iozone -o pid,comm` 查看进程号，
再执行 `kill -TERM <PID>`。随后执行 `wait` 等待后台输出结束，清理 `/root/iozone.tmp` 并 `sync`；
取消后的报告可能不完整，应记录为未完成。Zephyr 需要取消时，在其测量控制台按 Ctrl+C，等待返回 Shell。

人工检查工具输出中的错误、采样统计和完成状态，并保存对应结果文件。
三个工具的统计分别覆盖各自的运行区间；记录实际重叠范围和测量数值，本用例不设置性能数值门槛。

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
如果先前使用 `script` 录制，在返回的工作站录制 Shell 中执行 `exit`，关闭并保存串口日志。

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

功能场景要求三客户机同时运行且以上 7 项均通过。
性能场景还需人工记录三个负载的实际重叠、测量统计、完成状态和结果文件：

| 负载 | 记录内容 | 结果 |
| --- | --- | --- |
| StarryOS cyclictest | 采样数、Min/Avg/Max（微秒）、是否主动停止、持久化日志 | 待记录 |
| Linux IOZone | 各记录大小的顺序及随机读写速率、完成状态、`test.xls` | 待记录 |
| Zephyr ARM 定时器 | 每轮样本数、时延/间隔偏差的 Min/Avg/Max、单位、完成或超时状态 | 待记录 |
| 并发情况 | 各负载开始/结束顺序、每轮实际重叠范围、功能检查执行时机 | 待记录 |

并发性能测量应包含实际重叠的三个负载、有效统计及对应结果文件，性能数值原样记录。

## 配置维护

本目录直接验证标准镜像中的客户机；[三客户机 CI 配置](../../../../../test-suit/axvisor/normal/board-orangepi-5-plus/triple-guest-functional/build-aarch64-unknown-none-softfloat.toml)
验证工作区构建的 StarryOS，并使用独立的 Linux、Zephyr 固件路径，因此保留两套配置。
修改公共资源时同步核对 VM ID、CPU 绑定、内存范围、两份根盘；
Zephyr 的控制台型号、UART6 后端及 BIN/DTB 必须按各自固件匹配，不能直接互换。
