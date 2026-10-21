# AxVisor Zephyr 客户机功能检查

通过 AxVisor 启动单个 Zephyr 客户机，在调试串口观察启动信息和 Shell，手工查询版本、线程与设备。
本用例不自动输入命令或判定结果。

## 1. 运行条件

工作站需要 TGOSKits 的 AArch64 构建环境，板卡为 Orange Pi 5 Plus。
以下工作站命令均在仓库根目录执行，占位符替换为实际板卡服务信息。

### 1.1 标准镜像

板卡根文件系统使用标准镜像 `orangepi-5-plus.img.tar.xz`，其中已有配套文件，无需额外编译或传输：

| 文件 | 板端路径 |
| --- | --- |
| Zephyr 4.4.99 Shell | `/guest/zephyr/orangepi-5-plus` |
| 配套 DTB | `/guest/zephyr/orangepi-5-plus.dtb` |

单客户机和三客户机测试共用这两份原文件，不复制、不改名。

### 1.2 客户机配置

`configs/zephyr.toml` 定义 ID 1、单 vCPU、物理 CPU2、128 MiB 内存，入口为 `0x4000100c`。
配置不声明 `console0`，由原 DTB 提供 PL011 控制台地址 `0x09000000` 和中断号 33。
固件还初始化 UART6，本用例提供 `null` 后端的虚拟 UART6，没有物理设备直通。

`configs/build.toml` 的 `vm_configs` 打包客户机配置；`configs/board-shell.toml` 无自动检查步骤。
主机启动参数不设置 `axvisor.vm_start`，进入 AxVisor 后由人启动客户机。

## 2. 启动检查

修改配置后重新执行启动命令，旧会话中再次执行 `vm start` 不会读取工作站上修改的 TOML。

### 2.1 启动 AxVisor

在工作站交互终端执行，等待 `axvisor:/$`：

```bash
cargo xtask axvisor board \
  --config apps/axvisor/zephyr-vm-test/configs/build.toml \
  --board-config apps/axvisor/zephyr-vm-test/configs/board-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

### 2.2 启动客户机

在 AxVisor 中查看已打包的配置，ID 1 应在清单中；尚未创建时 `vm list` 为空是正常的：

```text
vm pool
vm start --console 1
```

观察 Zephyr 启动信息和 `ORANGEPI_ZEPHYR_SHELL_READY`，等待 `zephyr:~$` 后开始检查。
按 Ctrl+X 再按 h 返回 AxVisor；接回已运行的客户机使用 `vm console 1`。

## 3. Shell 功能检查

以下命令均在 Zephyr 提示符后逐条输入，等待命令结束再继续，保留每项实际输出。

### 3.1 内核版本

执行版本查询，应输出 Zephyr 版本号并返回提示符：

```text
kernel version
```

### 3.2 运行线程

执行线程查询，应包含 `shell_uart` 和 `idle` 并返回提示符；地址、状态和栈数据以实际输出为准：

```text
kernel thread list
```

### 3.3 设备列表

执行设备查询，应列出 `interrupt-controller@fe600000`、`serial@9000000`、
`serial@feb90000`，三者均为 `READY`，并返回提示符：

```text
device list
```

## 4. 收尾

三个查询均满足条件才算本用例通过。固件包含其他命令，本页不验证机器人、IVC 或性能。

### 4.1 停止客户机

按 Ctrl+X、h 返回 AxVisor，停止并删除本次客户机：

```text
vm stop 1
vm list
vm delete 1
exit
```

确认 `vm list` 中状态为 `stopped`，退出后等待 `Goodbye!`，再按 Ctrl+A、x 结束工作站连接并释放板卡。

### 4.2 问题记录

出现未知命令、Shell 无响应或必需设备未就绪时，保存启动日志和错误输出并记录失败项。
缺少文件时核对标准镜像及配置路径；没有 Shell 时核对使用的 BIN/DTB 是否为本页的配套文件。
工作站命令退出码为零仅表示会话结束，不能代替以上三项检查。
