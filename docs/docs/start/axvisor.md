---
sidebar_position: 1
sidebar_label: "AxVisor 启动"
title: "AxVisor 启动：AArch64 QEMU 与 Orange Pi 5 Plus"
---

# AxVisor 启动：AArch64 QEMU 与 Orange Pi 5 Plus

本文从进入已有 TGOSKits 仓库开始，使用本机已经准备好的构建工具，分别演示 QEMU 和 Orange Pi 5 Plus 的配置、编译与启动。

两个示例都以进入 **AxVisor 宿主管理 Shell** 并执行命令为目标。客户机配置列表保持为空，宿主使用内存根文件系统，不需要准备 Linux 客户机镜像或磁盘根盘。

先进入仓库，后续主机命令均在仓库根目录执行：

```bash
cd tgoskits
```

## 1. 启动流程

```text
defconfig 导出构建配置 → 修改配置 → build 编译内核
                                         ↓
                        qemu / board 根据启动配置运行内核
                                         ↓
                              AxVisor 管理 Shell
```

| 内容 | 作用 |
| --- | --- |
| 构建预设 | 仓库代码维护的一套初始构建参数，用名称选择 |
| 构建配置 TOML | 从预设导出后可编辑的文件，决定编译目标、驱动、日志等 |
| 启动配置 TOML | 决定 QEMU 参数，或远程板型、内核启动参数等 |
| 内核 ELF | 编译得到的可执行文件，是产物，不是配置文件 |

`build` 只编译和准备产物，不启动系统。`qemu`、`board` 使用构建配置准备内核，再按启动配置运行。先用 defconfig 选择构建配置，后续命令沿用这份选择；QEMU 和板卡的启动配置则在运行时显式指定。

## 2. 示例一：AArch64 QEMU

### 2.1 生成内核构建配置

```bash
cargo xtask axvisor config ls
cargo xtask axvisor defconfig qemu-aarch64
```

`config ls` 的三列分别是预设名称、平台 ID、Rust target。这里选择第一列的 `qemu-aarch64`。

`defconfig` 将预设展开，生成：

```text
tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml
```

执行 defconfig 后，工具会记住选择了哪份构建配置，后续命令可以省略配置路径。这里涉及两个不同的文件：

| 文件 | 内容与用途 |
| --- | --- |
| `tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml` | 真正的构建参数，例如功能、日志和 CPU 容量；需要调整构建时，修改这份文件 |
| `tmp/axbuild/.axvisor.toml` | 所选配置的路径、架构等默认选择记录；由工具维护，不需要手工编辑 |

完成 defconfig 后，可省略构建配置路径。例如：

```bash
cargo xtask axvisor build
```

工具会根据默认选择记录，找到刚才生成的构建配置。在刚完成上述 defconfig、没有附加其他覆盖参数的情况下，这相当于显式指定配置文件的完整命令：

```bash
cargo xtask axvisor build \
  --config tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml
```

两种写法读取的是同一份构建配置。`tmp/axbuild/.axvisor.toml` 只记录该文件的选择，不代替其中的构建参数；build 也不会启动内核。

下面直接编辑 defconfig 生成的文件。两个示例的 defconfig 写入同一路径，切换示例时会替换文件内容；从板卡示例切回 QEMU 时，重新执行 QEMU 的 defconfig 和配置修改步骤即可。

### 2.2 调整内核构建参数

打开 **`tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml`**，在根级增加 `max_cpu_num = 2`，将内核 CPU 容量设为 2，以匹配本例的 QEMU 配置。其余预设参数保持不变，完整内容如下：

```toml
target = "aarch64-unknown-none-softfloat"
features = ["ax-driver/nvme"]
rustflags = []
log = "Info"
max_cpu_num = 2
vm_configs = []

[env]

[runtime]
```

| 字段 | 设置说明 |
| --- | --- |
| `target` | 保持预设值，选择 AArch64 裸机平台契约；不要改成开发主机的 x86_64 target |
| `features` | 保留预设提供的 NVMe 驱动功能；是否实际挂磁盘由启动配置决定，编入驱动不等于必须挂磁盘 |
| `rustflags` | 保持空列表，不追加自定义编译选项 |
| `log` | Info 级别便于观察初始化过程，不改变是否启动客户机 |
| `max_cpu_num` | 将宿主内核的 CPU 容量设为 2，与下文 QEMU 的 2 个 CPU 对应；不是客户机 vCPU 数 |
| `vm_configs` | 保持空列表，本例不打包客户机，只启动 AxVisor Shell |
| `[env]`、`[runtime]` | 保持空表，不增加构建环境或任务栈、调度周期的覆盖值 |

`max_cpu_num` 等根级字段要写在 `[env]`、`[runtime]` 表头之前；写在表头下面会变成该表的成员，含义不同。

保存后查看工具实际解析的结果：

```bash
cargo xtask axvisor config show
```

确认解析结果中的 target、features、日志级别、CPU 容量和客户机列表与上述配置一致。

### 2.3 配置 QEMU 启动参数

用文本编辑器新建 **`tmp/axbuild/config/axvisor/qemu-aarch64.toml`**，填写下面内容。父目录已经由 defconfig 创建，无需另外创建目录。

```toml
args = [
  "-accel", "tcg",
  "-machine", "virt,virtualization=on,gic-version=3",
  "-cpu", "cortex-a72",
  "-smp", "2",
  "-m", "1G",
  "-nographic",
]
uefi = false
to_bin = true
cmdline = "console=ttyAMA0"
fail_regex = []
```

该文件指定 QEMU 的模拟硬件和内核启动方式：

| 配置 | 作用 |
| --- | --- |
| `-accel tcg` | 使用软件模拟，支持在 x86_64 开发主机上运行 AArch64，不要求 KVM |
| `-machine virt,...` | 使用 QEMU virt 平台，并启用 AxVisor 所需的虚拟化环境和 GICv3 中断控制器 |
| `-cpu cortex-a72` | 指定模拟的 CPU 型号 |
| `-smp 2` | 实际提供 2 个宿主 CPU，与构建容量对应 |
| `-m 1G` | 为虚拟机器分配 1 GiB 内存 |
| `-nographic` | 将控制台接到当前终端 |
| `uefi = false` | 使用本例的直接内核启动方式 |
| `to_bin = true` | 让运行器为该启动方式准备原始 BIN 文件 |
| `cmdline` | 传给 AxVisor 宿主的启动参数，选择该虚拟平台的 PL011 控制台 |
| `fail_regex = []` | 当前配置类型要求的空列表，不启用文本失败判定 |

文件没有磁盘参数、`root=` 或客户机启动选择，宿主从内存根启动；也没有自动检查步骤和测试总超时，进入 Shell 后保持交互。

运行时通过 `--qemu-config` 选择该文件，避免加载仓库默认 QEMU 配置中的磁盘设置。

### 2.4 编译内核

```bash
cargo xtask axvisor build
```

命令成功返回后，输出包含：

```text
[axbuild] cargo build elf=...
[axbuild] cargo build artifact_dir=...
```

`elf=` 后面给出本次内核 ELF 的实际路径，`artifact_dir=` 给出所在产物目录。在默认输出目录、未启用 debug 的本例中，ELF 位于：

```text
target/aarch64-unknown-linux-musl/release/axvisor
```

文件名虽然没有 `.elf` 后缀，内容仍是 ELF。构建配置使用裸机 target，axbuild 会为内核的标准库与链接流程选择实际编译 target，因此产物目录中的 `linux-musl` 不表示内核运行在主机 Linux 上。如果使用了自定义产物目录，以本次 `elf=` 输出定位。

这一步不会启动 QEMU。构建返回错误时，先处理第一条构建错误，不继续执行启动命令。

### 2.5 启动 QEMU 并进入 AxVisor Shell

```bash
cargo xtask axvisor qemu \
  --qemu-config tmp/axbuild/config/axvisor/qemu-aarch64.toml
```

构建配置沿用 defconfig 已选中的文件，也就是刚才手工修改的 `build-aarch64-unknown-none-softfloat.toml`。`--qemu-config` 单独指定模拟器启动配置，二者不会互相替代。

该命令会增量构建、准备运行器需要的 BIN，再启动 QEMU，不需要手工寻找 BIN 或输入加载地址。在默认目录中，QEMU 运行器准备的 BIN 为 `target/aarch64-unknown-linux-musl/release/axvisor.bin`；它是自动生成的运行产物，不需要编辑。

启动后应看到：

```text
Welcome to AxVisor Shell!
Type 'help' to see available commands
```

在 `axvisor:/$` 提示符后输入：

```text
help
echo QEMU_AARCH64_BOOT_OK
vm list
```

`help` 应列出命令，echo 应输出 `QEMU_AARCH64_BOOT_OK`，vm list 应输出 `No virtual machines found.`。空列表符合本例的 `vm_configs = []`。这些命令输入到 QEMU 内的 AxVisor Shell，而不是开发主机终端。

### 2.6 退出 QEMU

按 **Ctrl+A，松开，再按 x** 退出本例的 QEMU 控制台，回到开发主机终端。再次运行上面的 qemu 命令即可重新启动。

## 3. 示例二：Orange Pi 5 Plus

本例在开发主机编译，通过 `http://10.3.10.194:2999` 的板卡服务运行 Orange Pi 5 Plus。服务端管理实际板卡、串口、U-Boot、匹配的 DTB 和传输信息；开发主机需要有该服务的网络访问及板卡申请权限。

### 3.1 查询可用板卡

```bash
cargo xtask board ls --server http://10.3.10.194:2999
```

确认服务中存在 `OrangePi-5-Plus` 类型及可申请资源。这是查询命令，不占用板卡；不要先运行 board connect，因为它会另行占用一块板卡。

后续命令中的 endpoint 使用同一个地址。如果服务不可达或无申请权限，应先解决服务访问问题，内核构建配置不能解决此类错误。

### 3.2 生成板卡构建配置

```bash
cargo xtask axvisor defconfig orangepi-5-plus
```

工具将 Orange Pi 预设写入 **`tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml`**，并更新默认选择记录。这会替换前例同路径的 QEMU 构建配置；下面直接编辑这份新生成的文件，不需要创建目录或复制配置。

### 3.3 调整板卡构建参数

用文本编辑器打开 **`tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml`**，在根级增加 `max_cpu_num = 8`，将内核 CPU 容量设为 8，以匹配 RK3588 平台。其余板卡预设参数保持不变，完整内容如下：

```toml
target = "aarch64-unknown-none-softfloat"
features = [
  "ax-driver/rk3588-cpufreq",
  "ax-driver/rockchip-sdhci",
  "ax-driver/rockchip-dwmmc",
]
rustflags = []
log = "Info"
max_cpu_num = 8
vm_configs = []

[env]

[runtime]
```

| 字段 | 设置说明 |
| --- | --- |
| `target` | Orange Pi 5 Plus 是 AArch64 平台，保持预设值 |
| `features` | 保留 RK3588 CPU 频率和 Rockchip 存储控制器相关驱动，不使用 QEMU 的 NVMe 功能列表替代板卡预设 |
| `log` | Info 便于观察板卡初始化过程 |
| `max_cpu_num` | 设置宿主 CPU 容量为 8，与 RK3588 的 8 核平台对应 |
| `vm_configs` | 保持空列表，本例只启动 AxVisor 宿主 Shell |
| 其余字段 | rustflags 保持空列表，env/runtime 保持空表，不增加额外覆盖 |

同样将根级字段写在 `[env]`、`[runtime]` 之前，保存后查看解析结果：

```bash
cargo xtask axvisor config show
```

### 3.4 配置板卡启动参数

用文本编辑器新建 **`tmp/axbuild/config/axvisor/board-orangepi-5-plus.toml`**，填写下面内容。父目录已经由 defconfig 创建。

```toml
board_type = "OrangePi-5-Plus"
cmdline = "console=ttyS2,1500000 earlycon=uart8250,mmio32,0xfeb50000"
fail_regex = []
```

| 字段 | 作用 |
| --- | --- |
| `board_type` | 选择板卡服务中登记的类型；这里使用大写的服务类型名，不是 defconfig 的小写预设名 |
| `console=ttyS2,1500000` | 传给 AxVisor，选择该板的 UART2 控制台及波特率 |
| `earlycon=...` | 提供 UART2 的早期控制台类型和寄存器地址，用于观察启动早期日志 |
| `fail_regex = []` | 不设置文本失败判定，保持普通交互 |

该启动配置指定服务端板型和 AxVisor 的控制台参数。实际串口线路、DTB 和加载信息由服务端相应板卡资料提供；ttyS2 是板卡侧的控制台选择，不是开发主机的 `/dev/ttyUSB*` 路径。

本例不填写 `root=/dev/mmcblk0p2`，宿主不依赖指定的板卡分区；不填写 `axvisor.vm_start`，也不打包客户机，启动目标是 AxVisor Shell。文件没有 `shell_check_steps`、`[axbuild].checks` 和测试总 timeout，不会启动后自动执行测试或定时退出。

### 3.5 编译并启动 AxVisor

先只构建：

```bash
cargo xtask axvisor build
```

与 QEMU 示例相同，通过 `[axbuild] cargo build elf=` 输出确认本次 ELF 位置。默认目录仍为 `target/aarch64-unknown-linux-musl/release/axvisor`，但内容由当前 Orange Pi 构建配置生成。

两个示例的默认构建配置路径和 ELF 输出路径都相同，内容由当前所选预设及手工修改决定；不要仅凭路径相同就认为是同一份内核。下面的运行命令会沿用刚才选中的板卡构建文件增量构建，再准备对应产物。

构建成功后执行：

```bash
cargo xtask axvisor board \
  --board-config tmp/axbuild/config/axvisor/board-orangepi-5-plus.toml \
  --server http://10.3.10.194:2999
```

程序申请一块 OrangePi-5-Plus，准备传输副本，通过服务端启动链将本次内核交给 U-Boot 并启动。终端输出的 board_id 和会话标识是实际分配结果，应与本次日志一起保留。

板卡传输副本暂存在 `target/axbuild/board-elf/` 的临时子目录，运行结束后清理；持久 ELF 仍是构建日志打印的原文件。服务端处理本次启动所需的传输，不需要手工把 ELF 写入 SD/eMMC 或保存 U-Boot 环境。

看到 AxVisor Shell 欢迎信息和 `axvisor:/$` 后，输入：

```text
help
echo ORANGEPI_AXVISOR_BOOT_OK
vm list
```

应能看到命令列表、`ORANGEPI_AXVISOR_BOOT_OK` 和 `No virtual machines found.`。这些命令在板卡上的 AxVisor Shell 执行；看到 U-Boot 提示符或上传完成还没有达到本例的启动终点。

### 3.6 退出串口并释放板卡

在当前串口终端按 **Ctrl+A，松开，再按 x**，客户端退出串口并请求释放会话，随后返回开发主机终端。留意是否出现释放失败信息，再查询资源：

```bash
cargo xtask board ls --server http://10.3.10.194:2999
```

并发使用者会影响资源数量，不能仅凭数量判断本次释放结果。有明确释放错误时，保留实际 board_id、会话标识和错误交由服务维护者处理。板卡电源处理由服务端配置决定。

## 4. 配置文件与构建产物

下表路径均相对仓库根目录；产物使用默认 target 输出目录。

| 路径 | 来源与用途 |
| --- | --- |
| `tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml` | defconfig 导出并选中的构建配置；直接按当前示例手工修改。两个预设写同一路径，切换时重新生成和修改 |
| `tmp/axbuild/.axvisor.toml` | 工具保存的默认选择记录；不作为本页手工编辑对象 |
| `tmp/axbuild/config/axvisor/qemu-aarch64.toml` | 手工创建的 QEMU 启动参数；按 2.3 节填写 |
| `tmp/axbuild/config/axvisor/board-orangepi-5-plus.toml` | 手工创建的远程板卡启动参数；按 3.4 节填写 |
| `target/aarch64-unknown-linux-musl/release/axvisor` | 当前配置编译出的内核 ELF；不要编辑，用本次构建日志确认其身份和位置 |
| `target/aarch64-unknown-linux-musl/release/axvisor.bin` | QEMU 运行器准备的 BIN；自动使用，不手工修改 |
| `target/axbuild/board-elf/` | 板卡运行期间的临时传输副本目录；由工具管理并清理 |

本例的 `vm_configs` 为空，不生成客户机资源包。Shell 中 VM 列表为空是预期行为。

## 5. 确认启动结果

两个示例都要求构建成功、出现 AxVisor Shell，并能实际执行 help、echo 和 vm list；构建日志、模拟器启动或 U-Boot 的输出只能说明到了某个中间阶段。

| 现象 | 检查方法 |
| --- | --- |
| config show 与手工填写的字段不一致 | 检查文件路径、字段是否误写到 env/runtime 表下，以及是否额外传入了覆盖参数 |
| 构建失败 | 从本次输出的第一条编译错误处理，不继续运行旧产物 |
| 意外要求根盘或客户机镜像 | 检查是否用了本页启动文件、是否保持 vm_configs 为空、是否增加了 root 参数 |
| QEMU 没有可用 Shell | 核对机器虚拟化、GIC、CPU 数和控制台参数，保留启动日志 |
| 板卡服务不可达、无板卡或无权限 | 处理服务和资源条件，不修改内核来绕过 |
| 板卡串口乱码或停在 U-Boot | 核对该设备的服务端 DTB、串口、波特率和启动资料，保留实际 board_id 与首次错误 |

本页使用普通运行入口，自动功能测试由单独的 test 命令和测试配置负责，不混入这两个启动示例。
