# AxVisor ArceOS 客户机功能检查

## 测试用途

在板卡上通过 AxVisor 启动单个 ArceOS 客户机，由测试者手工输入命令，
检查客户机能否启动、Shell 能否列出命令、查询版本并正常退出。

仓库提供客户机源码、构建配置和本文档。按下文依次编译客户机、启动和检查，结果由测试者记录。

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
本用例需额外编译下面的 Shell 固件。`configs/arceos.toml` 使用 `${workspace}` 引用工作站编译产物，
构建 AxVisor 时将 BIN 和客户机配置一起打包，无需向板端复制文件。

### 首次准备

启动前先在工作站编译 Shell 固件；修改客户机源码后，重新编译并重新启动 AxVisor。

#### 1. 编译客户机并确认产物

```bash
cargo xtask arceos build -p arceos-vm-shell \
  --target aarch64-unknown-none-softfloat \
  --config apps/axvisor/normal/board-ballpick-robot/kernel-types/arceos/configs/guest-build.toml
```

确认镜像和配置文件存在，且大小非零：

```bash
ls -lh target/aarch64-unknown-linux-musl/release/arceos-vm-shell.bin \
  apps/axvisor/normal/board-ballpick-robot/kernel-types/arceos/configs/arceos.toml
```

#### 2. 确认镜像打包方式

`configs/arceos.toml` 使用 `${workspace}` 表示仓库根目录：

```toml
kernel_path = "${workspace}/target/aarch64-unknown-linux-musl/release/arceos-vm-shell.bin"
```

该路径指向工作站编译产物，不随配置文件的目录层级改变。
先确认上一步的 BIN 已生成，再执行下文的 AxVisor 启动命令。
构建工具会将 BIN 打包到 `/guest/builtin/images/`，并改写打包配置中的镜像路径。

### 配置说明

- `configs/guest-build.toml`：单 CPU 客户机构建，生成 bin 镜像。
- `configs/arceos.toml`：1 个 vCPU、128 MiB 内存，通过 `${workspace}/target/...` 引用工作站编译的 Shell BIN。
- `configs/build.toml`：通过 `vm_configs` 选择上述配置并打包其引用的 BIN，进入 AxVisor 后手工启动。
- `configs/uboot-shell.toml`：空自动检查步骤，保留人工串口会话。

修改客户机配置或重新编译 BIN 后，重新执行下文的 AxVisor 启动命令，将配置与镜像一起重新打包。
主机启动参数中应保持 `axvisor.vm_start` 未设置，避免客户机开机自动启动。

## 启动

在工作站交互终端、仓库根目录执行：

```bash
cargo xtask axvisor board \
  --config apps/axvisor/normal/board-ballpick-robot/kernel-types/arceos/configs/build.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/kernel-types/arceos/configs/uboot-shell.toml \
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
