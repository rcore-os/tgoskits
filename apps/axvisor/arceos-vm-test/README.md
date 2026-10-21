# AxVisor ArceOS 客户机功能检查

## 测试用途

在 Orange Pi 5 Plus 上通过 AxVisor 启动单个 ArceOS 客户机，由测试者手工输入命令，
检查客户机能否启动、Shell 能否列出命令、查询版本并正常退出。

仓库提供客户机源码、构建配置和本文档。按下文依次编译、放置文件、启动和检查，结果由测试者记录。

## 边界

- 使用 `cargo xtask axvisor board` 启动手工会话，不使用 `cargo xtask axvisor test` 执行本页检查。
- 客户机为 `guest_type = "virtualized"`，只提供 `help`、`uname`、`exit` 三条 Shell 命令。
- 本用例不进行 NPU、网络、文件读写或磁盘测试，不需要客户机根文件系统镜像。

## 运行前确认

先按仓库开发环境说明准备 Rust；PATH 中需要能找到
`aarch64-linux-musl-cc` 和 `aarch64-linux-musl-ar`。
本页工作站命令均从 TGOSKits 仓库根目录执行；所有板卡连接使用同一组板卡类型、服务地址和端口。

板卡根文件系统使用标准镜像 `orangepi-5-plus.img.tar.xz`。其中的
`/guest/arceos/orangepi-5-plus` 是网络程序，不提供本用例的 Shell 命令。
本用例需额外编译下面的 Shell 固件，不覆盖标准 ArceOS 文件。
板端只需手工部署客户机镜像 `/guest/axvisor/arceos/arceos-vm-shell.bin`。
客户机配置由 `vm_configs` 自动打包到 `/guest/builtin/configs/`，无需另行复制 TOML 到板端。

### 首次准备

每块板首次使用前完成以下步骤；之后修改了客户机源码，重新编译并更新板端镜像。

#### 1. 编译客户机并确认产物

```bash
cargo xtask arceos build -p arceos-vm-shell \
  --target aarch64-unknown-none-softfloat \
  --config apps/axvisor/arceos-vm-test/configs/guest-build.toml
```

确认镜像和配置文件存在，且大小非零：

```bash
ls -lh target/aarch64-unknown-linux-musl/release/arceos-vm-shell.bin \
  apps/axvisor/arceos-vm-test/configs/arceos.toml
```

#### 2. 连接板卡原生 Linux

替换下面的占位值，连接并持有板卡：

```bash
cargo xtask board connect -b <板卡类型> --server <服务地址> --port <端口>
```

登录板卡原生 Linux 后查询 IP：

```bash
ip -4 -br address
```

记下工作站能够访问的板卡 IP。保持终端 A 的连接，接着在另一个终端传输文件。
板卡 IP 通常只在 ostool 会话持有期间可达；不要同时运行 `board connect` 和 `axvisor board`。

#### 3. 传输并放置文件

以下以板端用户 `orangepi` 为例。终端 B 在仓库根目录执行，替换实际板卡 IP：

```bash
board_ip=<板卡IP>
scp target/aarch64-unknown-linux-musl/release/arceos-vm-shell.bin "orangepi@$board_ip:/tmp/"
ssh "orangepi@$board_ip"
```

进入板卡 SSH 后执行。先前传入 `/tmp/` 的镜像按原文件名安装：

```bash
sudo install -d -m 755 /guest/axvisor/arceos
sudo install -m 644 /tmp/arceos-vm-shell.bin /guest/axvisor/arceos/arceos-vm-shell.bin
sync
ls -lh /guest/axvisor/arceos/
exit
```

确认目录中有非空的 `arceos-vm-shell.bin`。`exit` 退出 SSH，返回工作站；
终端 A 仍持有板卡，可以继续下面的启动前核对。

### 启动前核对

每次启动 AxVisor 前，在板卡原生 Linux 中执行以下命令。
若准备时的连接已经退出，先在工作站重新连接：

```bash
cargo xtask board connect -b <板卡类型> --server <服务地址> --port <端口>
```

进入板卡 Linux 后执行：

```bash
ls -lh /guest/axvisor/arceos/arceos-vm-shell.bin
findmnt -T /guest/axvisor/arceos
```

确认镜像大小非零，且位于 AxVisor 能挂载的板卡 eMMC 文件系统中。

核对完成后，在 `board connect` 终端按 **Ctrl+A**，再按 **x**，等待命令退出并释放板卡，
再执行下文的 AxVisor 启动命令。

### 配置说明

- `configs/guest-build.toml`：单 CPU 客户机构建，生成 bin 镜像。
- `configs/arceos.toml`：1 个 vCPU、128 MiB 内存，镜像路径为 `/guest/axvisor/arceos/arceos-vm-shell.bin`。
- `configs/build.toml`：通过 `vm_configs = ["apps/axvisor/arceos-vm-test/configs/arceos.toml"]` 打包客户机配置，进入 AxVisor 后手工启动。
- `configs/board-shell.toml`：空自动检查步骤，保留人工串口会话。

修改客户机配置后，重新执行下文的 AxVisor 启动命令即可重新打包配置，无需手工传输 TOML。
更换登录用户名时，替换 SSH/SCP 命令中的 `orangepi`，板端镜像路径保持不变。
主机启动参数中应保持 `axvisor.vm_start` 未设置，避免客户机开机自动启动。

## 启动

在工作站交互终端、仓库根目录执行：

```bash
cargo xtask axvisor board \
  --config apps/axvisor/arceos-vm-test/configs/build.toml \
  --board-config apps/axvisor/arceos-vm-test/configs/board-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

看到 `axvisor:/$` 后，查看可用配置：

```text
vm pool
```

应在 `Builtin VM catalog:` 下看到 ID 为 `1`、名称为 `arceos-vm-test` 的条目，状态为
`not-created`。此时 `vm list` 为空是正常的，因为客户机尚未创建。

手工创建并启动该 builtin 客户机，接入其控制台：

```text
vm start --console 1
```

该命令使用打包的配置完成创建和启动；以下命令中的 `1` 对应 `configs/arceos.toml` 的 `id = 1`。

看到 ArceOS 启动日志及 `arceos:/$` 提示符后，执行下面的检查。
创建或启动失败时先处理错误，不继续使用其他已有 VM 的输出。

按键约定：**Ctrl+X** 后按 **h** 返回 AxVisor 控制台；**Ctrl+A** 后按 **x** 结束 ostool
串口会话。返回 AxVisor 控制台后若需重新进入本客户机，执行 `vm console 1`。

## 场景 1：Shell 功能检查

在客户机的 `arceos:/$` 提示符后逐条执行。每项按实际输出独立记录结果。

### 功能项 1：列出支持的命令

输入：

```text
help
```

预期输出：

```text
Available commands:
  help
  uname
  exit
arceos:/$
```

通过条件：列出 `help`、`uname`、`exit`，并再次出现 `arceos:/$` 提示符。

### 功能项 2：查询版本

输入：

```text
uname
```

预期输出示例：

```text
ArceOS 0.1.0 aarch64
arceos:/$
```

通过条件：输出包含 `ArceOS`、本次客户机应用的版本号及 `aarch64`，并再次出现 Shell 提示符。
版本号以 `guest/Cargo.toml` 为准；架构后可能附带平台名称。

### 判定

两项都满足各自通过条件，才算本场景通过。命令无输出、返回未知命令或 Shell 无法继续响应时，
记录对应项失败及实际输出。

## 结束

在客户机中输入：

```text
exit
```

预期输出 `Bye~`。按 **Ctrl+X**，再按 **h** 返回 AxVisor，查看客户机是否已经停止：

```text
vm list
```

若客户机仍在运行，执行：

```text
vm stop 1
vm list
```

确认本次客户机的状态为 `stopped` 后，可删除它：

```text
vm delete 1
```

最后按 **Ctrl+A**，再按 **x** 退出串口，等待工作站命令结束并释放板卡。
记录退出检查的结果：是否看到 `Bye~`，以及不执行 `vm stop` 时客户机是否自行停止。

## 判定边界

分别记录客户机启动、`help`、`uname` 和 `exit` 的结果。本页通过表明这个精简 ArceOS
客户机能够启动并完成上述交互，不涵盖其他 ArceOS 应用、网络、存储或性能验收。

工作站命令正常退出只表示会话结束，不能代替这些手工检查结果。
