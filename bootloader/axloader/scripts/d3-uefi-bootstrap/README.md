# D3 无人值守 UEFI 启动装配

本目录提供一套可版本化、可重复执行的 D3 USB 启动装配源。它复用固定版本的 EDK2 Shell 自动执行同目录 startup.nsh，装载临时网络驱动后调用原 axloader launcher；不修改 Rust、A/B、STATE 或固件配置。

## 1. 目标与边界

本装配只解决 D3 诊断 USB 的冷启动编排和可回滚组装，不扩大 axloader 或 Axvisor 的运行时能力。自动入口与驱动脚本均为源码模板，USB 内容由装配脚本从显式输入生成。

### 1.1 常驻 USB 启动流程

该流程把零等待 EDK2 Shell 放在 EFI/BOOT，并由同目录 [startup.nsh](startup.nsh) 通过 homefilesystem 找到当前 USB。脚本先加载 RNG、Hash2，再调用 [load-network.nsh](load-network.nsh) 的 no-report 模式，最后从 EFI/AXLOADER/LAUNCHER.EFI 启动原 launcher。

自动启动不依赖 USB 可写，不运行 check-network.nsh，不写 NVRAM，不自动重启，也不扫描其他文件系统。普通无参数运行 load-network.nsh 时仍保留诊断查询和日志行为。

### 1.2 明确的非目标

本目录不构建 Shell、不构建 launcher、不下载驱动，也不访问或格式化块设备。A/B 文件、STATE、OTA 清单和 BIOS 设置不由装配脚本复制、重建或重置。

该方案仅面向当前 D3 诊断介质。真实 D3 冷启动、Realtek NII、服务端下发 Axvisor 和完整板卡启动链仍由主代理做整链验证。

## 2. 启动源码

启动源码分为“如何使用自动 entrypoint”和“被调用的驱动模板”两部分。它们共同保证路径来自启动 USB，并且自动路径没有日志写依赖。

### 2.1 自动 startup

[startup.nsh](startup.nsh) 打印 `[D3-AUTOBOOT]`、`%0` 和 homefilesystem，先检查同卷 RNG、Hash2、load 脚本和 launcher 是否存在。冷启动 Shell 可能尚未选定当前文件系统，因此脚本先执行 homefilesystem 对应的卷映射（如 `FS0:`），检查返回状态后再 `cd \EFI\D3NET`。进入目录后，如果存在可选 `PciSioSerialDxe.efi`，脚本先加载它，再做 RNG/Hash2/网络装配。任一前置路径或加载步骤失败都会保留状态并退出当前脚本，不会继续进入 launcher。

Shell 的 startup 查找逻辑优先使用 Shell image 同目录文件，并设置 homefilesystem；脚本因此不使用 FS0、FS3 或 PATH。若 launcher 返回，startup 只打印返回状态并停止，不递归启动 BOOTX64 或其他盘。

### 2.2 网络模板

[load-network.nsh](load-network.nsh) 是唯一正式网络装配模板。无参数运行时依次加载 DPC、SNP、MNP、ARP、IP4、UDP4、DHCP4、TCP，执行 `connect -r`，随后运行 check-network.nsh；传入 `no-report` 时跳过查询和日志。可选参数使用带哨兵前缀的 `x%1 == xno-report` 比较，避免无参数时出现空左操作数。

自动启动使用 `no-report`，因此 USB 只读或日志文件问题不会阻断已经成功的网络装配。装配器始终复制本目录模板，不会使用较旧诊断包中的 load-network.nsh。

### 2.3 可选标准 UART

`PciSioSerialDxe.efi` 是第 11 个可选驱动，不替代 10 个必要驱动。它只使用官方 `MdeModulePkg/Bus/Pci/PciSioSerialDxe` 的自动发现逻辑，匹配标准 SIO `PNP0501` 或 PCI 16550 类设备，并从父设备资源取得地址；不硬编码 0x3F8，也不添加猜测性 VID/DID/BAR/offset/clock PCD。该路径不使用 `SerialDxe` 或固定 `SerialPortLib`。

该驱动沿用官方全局 `PcdSerialClockRate` 默认值 1,843,200 Hz。这要求固件资源对应设备实际使用 1.8432 MHz UART 输入时钟；当前 D3 尚未证明满足该条件，仍需要实机验收。非标准 UART、无 `Sio`/`PciIo` 父对象、或真实时钟不同的设备不能仅凭驱动加载成功就认为可用。

startup 只在该文件存在时执行 `load -nc`，不在这里额外执行 connect。后续 load-network 的统一 `connect -r` 才会让驱动尝试绑定；load 成功不等于已经发布 SerialIo。主机侧 CP2102 只是串口线路，不代表 D3 固件具备 UART handle。多个 SerialIo 仍由 loader 按 ConOut 路径或唯一 handle 规则选择，不能随机绑定。

整个可选驱动加载和统一 connect 都在 launcher 启动前完成。loader 的网络接口选定后才创建 SerialBeacon，[serial.rs](../../src/loader/serial.rs#L82) 创建 beacon 时，[loader/mod.rs](../../src/loader/mod.rs#L48) 已能看到本轮产生的 SerialIo handle。

## 3. 零等待 Shell

Shell 必须从固定上游提交重新构建，并在构建时覆盖 PCD；现成发行版 Shell 不能仅靠放置 startup.nsh 获得零等待行为。

### 3.1 固定提交

使用 edk2-stable202502 提交 `fbe0805b2091393406952e84724188f8c1941837`。构建产物在启动时读取 PcdShellDefaultDelay，只有构建期值为 0 才会跳过等待并优先执行同目录 startup。

### 3.2 精确构建命令

在已配置好的 edk2 工作区执行以下命令。该命令不写 BIOS 或 NVRAM，只生成用于 USB EFI/BOOT 的 Shell 文件。

```sh
build \
  -p ShellPkg/ShellPkg.dsc \
  -a X64 \
  -t GCC5 \
  -b RELEASE \
  -m ShellPkg/Application/Shell/Shell.inf \
  --pcd gEfiShellPkgTokenSpaceGuid.PcdShellDefaultDelay=0
```

主代理实际构建产物位于：

```text
Build/Shell/RELEASE_GCC5/X64/ShellPkg/Application/Shell/EA4BB293-2D7F-4456-A681-1F22F42CD0BC/OUTPUT/Shell.efi
```

### 3.3 可选 UART 驱动构建

UART 支持是独立可选产物，不属于 Shell 构建。主代理可在同一临时 NetworkPkg DSC 中构建官方驱动：

```sh
build \
  -p NetworkPkg/NetworkPkg.dsc \
  -a X64 \
  -t GCC5 \
  -b RELEASE \
  -m MdeModulePkg/Bus/Pci/PciSioSerialDxe/PciSioSerialDxe.inf
```

产物放入 `EFI/D3NET/PciSioSerialDxe.efi` 后才会进入可选加载路径；不放置该文件时，原有 10 驱动自动流程保持不变。

## 4. 装配输入与输出

[assemble-autoboot.sh](assemble-autoboot.sh) 接收 Shell、launcher、内部 D3NET 目录和全新输出目录。--d3net 明确表示内层 `EFI/D3NET`，不是诊断包根目录。

### 4.1 必需输入

--d3net 必须包含 10 个必要 EFI 和 check-network.nsh。缺少任一必要文件时装配器明确失败；旧诊断包的 load-network.nsh 会被本目录正式模板覆盖。可选的 PciSioSerialDxe.efi 通过目录整体复制进入输出，不要求存在，也不增加装配参数。

本目录的 [check-network.nsh](check-network.nsh) 是版本化查询源码，不是 EDK2 构建产物。若 target 的诊断目录没有该脚本，应先手工复制：

```sh
cp bootloader/axloader/scripts/d3-uefi-bootstrap/check-network.nsh \
  target/axbuild/axloader/d3-network-diagnostic-rng/EFI/D3NET/
```

```sh
./bootloader/axloader/scripts/d3-uefi-bootstrap/assemble-autoboot.sh \
  --shell /absolute/path/to/zero-delay/Shell.efi \
  --launcher /absolute/path/to/original/LAUNCHER.EFI \
  --d3net target/axbuild/axloader/d3-network-diagnostic-rng/EFI/D3NET \
  --output /absolute/path/to/new-usb-root
```

输出目录不存在时创建；存在但非空时拒绝，不覆盖已有内容。复制失败以非零状态退出，部分输出不会自动清理，重试应使用新的空目录。

### 4.2 输出结构

装配结果是可直接部署到 USB 根目录的文件树。EFI/BOOT 保存零等待 Shell、自动 startup 和原入口备份，EFI/AXLOADER 保存运行 launcher，EFI/D3NET 保存驱动与正式网络模板。

```text
EFI/BOOT/BOOTX64.EFI
EFI/BOOT/BOOTX64.PREVIOUS.EFI
EFI/BOOT/startup.nsh
EFI/AXLOADER/LAUNCHER.EFI
EFI/D3NET/DpcDxe.efi
EFI/D3NET/SnpDxe.efi
EFI/D3NET/MnpDxe.efi
EFI/D3NET/ArpDxe.efi
EFI/D3NET/Ip4Dxe.efi
EFI/D3NET/Udp4Dxe.efi
EFI/D3NET/Dhcp4Dxe.efi
EFI/D3NET/TcpDxe.efi
EFI/D3NET/RngDxe.efi
EFI/D3NET/Hash2DxeCrypto.efi
EFI/D3NET/PciSioSerialDxe.efi  # optional
EFI/D3NET/load-network.nsh
EFI/D3NET/check-network.nsh
```

## 5. Launcher 与 A/B 语义

本方案只改变 launcher 的启动位置，不改变其槽选择和 OTA 逻辑。launcher 仍从自身的 LoadedImage 设备打开同一卷 EFI/AXLOADER 路径。

### 5.1 原 launcher 入口

[launcher.rs](../../src/launcher.rs#L44) 继续调用 OtaDisk 和 load_slot；[OtaDisk](../../src/ota/disk.rs#L24) 使用卷绝对 A/B 路径，并从 launcher 的 LoadedImage device 加载槽文件。因此从 EFI/AXLOADER/LAUNCHER.EFI 运行不会绕过 A/B、pending、attempted、摘要校验或回滚。

startup 不直接执行 A.EFI 或 B.EFI，也不扫描其他磁盘。原 launcher 文件名改变但所在卷和目录保持约定，A/B 独占关系不变。

### 5.2 状态安装边界

装配器只生成 EFI/BOOT、EFI/AXLOADER/LAUNCHER.EFI 和 EFI/D3NET 内容，不创建 A/B 或 STATE。主代理安装到已有 USB 时必须逐文件保留原 A/B、STATE、OTA 清单及其他启动状态；QEMU 测试状态由既有工具单独初始化。

## 6. 失败与诊断

自动启动优先保证失败可见且不越级进入 launcher。缺文件、load 失败或 connect 失败都会在 Shell 中停止，并显示失败对象和已保存的 UEFI 状态。

### 6.1 自动启动失败路径

startup 在加载前检查 RNG、Hash2、load 模板路径和 launcher。可选 PciSioSerialDxe 存在但 load 失败、RNG/Hash2 失败或 load-network no-report 失败，都不会调用 launcher；可选文件缺失不会阻断原路径。Shell 的 connect -r 返回成功也不替代 launcher 后续对同一 NIC handle 四协议的实际检查。

### 6.2 普通诊断模式

在 EFI/D3NET 手工运行无参数 `load-network.nsh` 仍会在网络装配后调用 check-network.nsh，并收集 ASCII 日志。自动启动使用 no-report，不依赖 USB 可写；需要日志时应在可写介质上单独执行诊断脚本。

## 7. 安装、回滚与验证

安装前应备份目标 USB，尤其是原 BOOTX64.EFI、A/B、STATE 和 OTA 状态。BIOS 只需人工把 USB 设置为首选启动项一次，不需要刷写固件。

### 7.1 恢复原入口

BOOTX64.PREVIOUS.EFI 是装配输入中原 launcher 的恢复副本。先进入 USB 对应的 UEFI Shell，并选择该 USB 卷：

```text
FSx:
cp EFI\BOOT\BOOTX64.PREVIOUS.EFI EFI\BOOT\BOOTX64.EFI
```

将 FSx 替换为 map 输出中实际 USB 映射。恢复后可回到原 launcher 入口。若不希望 Shell 再次自动执行，应先备份并移走 EFI/BOOT/startup.nsh。

### 7.2 待完成验证

主代理需要使用真实 FAT 和第二个文件系统验证 homefilesystem、零人工输入、失败停止、launcher 返回、A/B/STATE 不变及 BOOTX64.PREVIOUS.EFI 恢复。额外验证 PciSioSerialDxe 的可选路径时，还需确认 1.8432 MHz 默认适用、确实产生可用 SerialIo，并验证多 UART 时的 ConOut/唯一 handle 选择。真实 D3 的 AMD 固件、Realtek NII、Axvisor 下发和完整板卡启动尚未验证，本目录不代表整链已经通过。

## 8. EDK2 可复现构建

本章从固定上游版本重建本装配依赖的 Shell、网络驱动和可选 UART 驱动。所有命令都需要在对应 edk2 工作区手工执行；本目录不提供自动下载、自动安装或构建包装器。主机需预先具备 `python3`、`gcc`、`g++`、`make`、`nasm` 和 `git`。

### 8.1 固定版本与子模块

请使用 edk2-stable202502 提交 `fbe0805b2091393406952e84724188f8c1941837`。该提交包含本补丁所基于的 NetworkPkg、SecurityPkg 和 ShellPkg 行号与文件内容。

```sh
export PYTHON_COMMAND=python3
git checkout fbe0805b2091393406952e84724188f8c1941837
git submodule update --init --recursive
```

递归初始化子模块会下载完整依赖，耗时和磁盘占用可能较大。不得在未匹配的 edk2 提交上强行应用本目录 [edk2-d3.patch](edk2-d3.patch)。

### 8.2 BaseTools 与补丁

本补丁包含 NetworkPkg 组件/库映射、CTR-only Rng 源码和 RngDxe INF 的必要最小修改。先构建 BaseTools、初始化 edk2 环境，再检查补丁能否精确应用。

```sh
make -C BaseTools
source edksetup.sh
git apply --check /absolute/path/to/d3-uefi-bootstrap/edk2-d3.patch
git apply /absolute/path/to/d3-uefi-bootstrap/edk2-d3.patch
```

`git apply --check` 失败时必须停止，不得使用 `--3way`、强制覆盖或手工猜测版本差异。需要检查补丁可逆性时，可在应用后执行：

```sh
git apply --reverse --check /absolute/path/to/d3-uefi-bootstrap/edk2-d3.patch
```

### 8.3 网络与 UART 模块

以下公共参数关闭 IPv6、TLS、HTTP、HTTP Boot、iSCSI、PXE 和 VLAN。该组合让构建聚焦 D3 所需的 IPv4 网络栈和标准 SIO/PCI UART。

```bash
NETWORK_FLAGS=(
  -D NETWORK_IP6_ENABLE=FALSE
  -D NETWORK_TLS_ENABLE=FALSE
  -D NETWORK_HTTP_ENABLE=FALSE
  -D NETWORK_HTTP_BOOT_ENABLE=FALSE
  -D NETWORK_ISCSI_ENABLE=FALSE
  -D NETWORK_PXE_BOOT_ENABLE=FALSE
  -D NETWORK_VLAN_ENABLE=FALSE
)
```

9 个网络与 Hash 产物由 8 个核心模块和 Hash2DxeCrypto 组成；另构建 CTR-only RngDxe 和可选 PciSioSerialDxe：

| INF | 产物 |
|---|---|
| `NetworkPkg/DpcDxe/DpcDxe.inf` | `DpcDxe.efi` |
| `NetworkPkg/SnpDxe/SnpDxe.inf` | `SnpDxe.efi` |
| `NetworkPkg/MnpDxe/MnpDxe.inf` | `MnpDxe.efi` |
| `NetworkPkg/ArpDxe/ArpDxe.inf` | `ArpDxe.efi` |
| `NetworkPkg/Ip4Dxe/Ip4Dxe.inf` | `Ip4Dxe.efi` |
| `NetworkPkg/Udp4Dxe/Udp4Dxe.inf` | `Udp4Dxe.efi` |
| `NetworkPkg/Dhcp4Dxe/Dhcp4Dxe.inf` | `Dhcp4Dxe.efi` |
| `NetworkPkg/TcpDxe/TcpDxe.inf` | `TcpDxe.efi` |
| `SecurityPkg/Hash2DxeCrypto/Hash2DxeCrypto.inf` | `Hash2DxeCrypto.efi` |
| `SecurityPkg/RandomNumberGenerator/RngDxe/RngDxe.inf` | `RngDxe.efi` |
| `MdeModulePkg/Bus/Pci/PciSioSerialDxe/PciSioSerialDxe.inf` | `PciSioSerialDxe.efi`，可选 |

每项使用相同命令模式，只替换 `-m` 的 INF：

```bash
build \
  -p NetworkPkg/NetworkPkg.dsc \
  -a X64 \
  -t GCC5 \
  -b RELEASE \
  "${NETWORK_FLAGS[@]}" \
  -m NetworkPkg/DpcDxe/DpcDxe.inf
```

上述 11 个模块的最终 EFI 复制到：

```text
Build/NetworkPkg/RELEASE_GCC5/X64/<ModuleName>.efi
```

`DxeNetLib` 等库由模块链接生成，不单独放入 EFI/D3NET。`check-network.nsh` 由本目录源码提供，不由 EDK2 构建产生。

### 8.4 零等待 Shell

Shell 必须单独使用 ShellPkg 构建。选择全部命令集成且 FILE_GUID 为 `EA4BB293-2D7F-4456-A681-1F22F42CD0BC` 的产物，不选择普通 `7C04A583-...` Shell。

```sh
build \
  -p ShellPkg/ShellPkg.dsc \
  -a X64 \
  -t GCC5 \
  -b RELEASE \
  -m ShellPkg/Application/Shell/Shell.inf \
  --pcd gEfiShellPkgTokenSpaceGuid.PcdShellDefaultDelay=0
```

供装配器使用 `--shell` 的稳定命名副本为：

```text
Build/Shell/RELEASE_GCC5/X64/Shell_EA4BB293-2D7F-4456-A681-1F22F42CD0BC.efi
```

也可使用该组件下 `OUTPUT/Shell.efi`。Shell 构建不写 NVRAM、不刷 BIOS；BIOS 只需人工配置 USB 优先一次。

### 8.5 组装前检查

完成构建后，`--d3net` 目录应包含 10 个必要 EFI 和查询脚本。将本目录的 check-network.nsh 手工复制进去，然后运行装配器；可选 UART 驱动存在时会由目录整体复制进入输出。

```sh
cp bootloader/axloader/scripts/d3-uefi-bootstrap/check-network.nsh \
  target/axbuild/axloader/d3-network-diagnostic-rng/EFI/D3NET/

./bootloader/axloader/scripts/d3-uefi-bootstrap/assemble-autoboot.sh \
  --shell /absolute/path/to/Shell_EA4BB293-2D7F-4456-A681-1F22F42CD0BC.efi \
  --launcher /absolute/path/to/original/LAUNCHER.EFI \
  --d3net target/axbuild/axloader/d3-network-diagnostic-rng/EFI/D3NET \
  --output /absolute/path/to/new-usb-root
```

装配器要求 `--d3net` 已经存在查询脚本，不会从本目录自动复制。当前 D3 实机冷启动、SerialIo 绑定和 Axvisor 下发仍未验收。
