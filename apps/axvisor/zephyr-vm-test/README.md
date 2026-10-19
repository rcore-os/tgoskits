# AxVisor Zephyr 客户机功能检查

## 测试用途

在 Orange Pi 5 Plus 上通过 AxVisor 启动单个 Zephyr 客户机，在调试串口观察启动信息和
Shell，手工查询内核版本、运行线程与设备。每项结果由测试者根据命令输出记录。

## 边界

- 使用 `cargo xtask axvisor board` 启动 AxVisor，再手工启动客户机，不自动输入或判定测试命令。
- 客户机为单 vCPU、128 MiB 内存的全虚拟化客户机，没有物理设备直通。
- 本用例只检查 Shell 交互，不运行机器人控制、网络或延迟基准。

### 启动前核对

每次启动 AxVisor 前，在板卡原生 Linux 中执行；若准备时的终端 A 已退出，先重新运行上面的
`cargo xtask board connect`。

```bash
ls -lh /guest/axvisor/zephyr/zephyr.bin /guest/axvisor/zephyr/zephyr.dtb
findmnt -T /guest/axvisor/zephyr
```

确认两份文件非空，位于 AxVisor 可挂载的板端 eMMC 文件系统中。核对后执行 `sync`，
按 **Ctrl+A** 再按 **x**，等待连接命令退出、板卡释放后，再启动 AxVisor。

### 配置说明

- `configs/zephyr.toml`：客户机 ID 为 `1`，绑定物理 CPU2，入口为 `0x4000100c`，镜像和 DTB
  位于 `/guest/axvisor/zephyr/`。`console0` 使用 DTB 指定的 PL011 地址与中断，接到调试串口。
  固件还初始化 UART6，本用例为它提供 `null` 后端的虚拟串口，不直通物理 UART6。
- `configs/build.toml`：通过 `vm_configs` 自动打包客户机配置；不需要手工复制 TOML 到板端。
- `configs/board-shell.toml`：提供板卡类型默认值，服务端地址与端口由命令行传入；没有自动检查步骤。

主机启动参数保持 `axvisor.vm_start` 未设置，进入 AxVisor 后再手工启动。修改客户机配置后
重新执行下面的启动命令即可重新打包，无需另外维护板端 TOML。

## 启动

在工作站交互终端、仓库根目录执行，替换全部占位值：

```bash
cargo xtask axvisor board \
  --config apps/axvisor/zephyr-vm-test/configs/build.toml \
  --board-config apps/axvisor/zephyr-vm-test/configs/board-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

看到 `axvisor:/$` 后查看可用配置：

```text
vm pool
```

`Builtin VM catalog:` 下应有 ID `1`、名称 `zephyr-vm-test`、状态 `not-created` 的条目。
此时 `vm list` 为空是正常的。手工创建、启动并接入客户机控制台：

```text
vm start --console 1
```

观察 Zephyr 启动 banner 和 `ORANGEPI_ZEPHYR_SHELL_READY`，等待 `zephyr:~$` 提示符，
再执行下面的检查。
如果输出基准结果或 `PROJECT EXECUTION SUCCESSFUL` 后没有 Shell，应重新核对镜像，
不能将基准执行成功视为本页检查通过。

按 **Ctrl+X** 再按 **h** 返回 AxVisor 控制台；重新接入已运行的客户机时执行 `vm console 1`。

## 场景 1：Zephyr Shell 功能检查

以下命令在客户机 `zephyr:~$` 后逐条输入。

### 功能项 1：内核版本

```text
kernel version
```

预期输出示例，版本以实际镜像为准：

```text
Zephyr version 4.4.99
zephyr:~$
```

通过条件：返回 Zephyr 版本号，并再次出现 Shell 提示符。

### 功能项 2：运行线程

```text
kernel thread list
```

预期包含线程列表，例如：

```text
Threads:
... shell_uart ...
... idle ...
```

通过条件：能列出线程及相关信息，包含 Shell 线程与空闲线程，命令结束后返回提示符。
线程地址、状态和栈数据以实际输出为准，不要求固定值。

### 功能项 3：设备列表

```text
device list
```

预期包含本镜像使用的虚拟中断控制器和串口：

```text
devices:
- interrupt-controller@fe600000 (READY)
- serial@9000000 (READY)
```

通过条件：能够列出设备，以上两个设备均为 `READY`，并返回 Shell 提示符。
实际列表可能包含其他设备。

### 判定

三项分别记录实际输出，全部满足对应条件才算本场景通过。
出现未知命令、缺少输出、Shell 无响应，或必需设备未就绪时，记录失败项和错误信息。

## 结束

按 **Ctrl+X** 再按 **h** 返回 AxVisor，停止本次客户机：

```text
vm stop 1
vm list
```

确认状态为 `stopped` 后可删除：

```text
vm delete 1
```

在 AxVisor 中执行 `exit`，等待显示 `Goodbye!`，再按 **Ctrl+A** 后按 **x**，
等待工作站命令退出并释放板卡。

## 判定边界

本页通过只表明本次 Zephyr Shell 镜像能在 AxVisor 中启动、响应三项查询并识别其虚拟设备。
固件包含额外的 IVC、机器人和延迟测试命令，本页只执行上述三项查询；UART6 没有物理输出。
不代表机器人控制、IVC 通信、物理设备直通或性能验收通过。
工作站命令退出码为零只表示会话正常结束，不代替手工检查结果。
