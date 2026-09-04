# Orange Pi 5 Plus StarryOS 实板调试复盘（2026-08-09）

> 环境：Orange Pi 5 Plus（RK3588、eMMC）、Ubuntu Focal rootfs、StarryOS、Orange Pi 原厂 U-Boot、1500000 波特率调试串口。
> 关联文档：[故障排查与恢复指南](board-orangepi-5-plus-troubleshooting.md)、
> [实板刷写参考](board-flash-rk3588-visionfive2.md)。

## 1. 最终状态

截至 2026-08-09，本轮调试得到以下结果：

- StarryOS 可以从 eMMC 上的 FIT 镜像启动。
- 已越过早期的 `Found physical memory regions:` 卡死点。
- 可以进入交互式控制台：`root@starry:/root #`。
- RTL8125 网络已工作，实测地址为 `192.168.6.133`，ICMP 可达。
- StarryOS 不再在开机后自动运行 grouped tests。
- OpenSSH 和 Dropbear 均无法完成 SSH 握手，当前只能使用调试串口操作。
- 原生 Ubuntu 下 CH340 在 1500000 波特率时存在大量乱码和命令丢字；同一硬件过去在 WSL/Windows 驱动链路下正常。

因此，本轮已经解决“StarryOS 无法进入控制台”和“如何稳定从 eMMC 启动”的问题；SSH 仍是独立的内核兼容性问题，不能通过改密码、密钥或 `sshd_config` 解决。

## 2. 问题与处理时间线

| 阶段 | 现象 | 根因或结论 | 处理 |
|---|---|---|---|
| 1 | 停在 `Found physical memory regions:` | RK3588 DTB 提供的 MMIO 区域超过固定容量 | 扩大 `axplat-dyn` 和 `axhal` 的内存区域容量 |
| 2 | 怀疑 8 核 SMP 导致卡死 | 将 CPU 数降为 1 后仍需继续排查；SMP 不是本次根因 | `max_cpu_num = 1` 仅作为隔离变量保留 |
| 3 | 启动后自动跑测试 | grouped tests 被 profile/init 自动触发 | 取消 profile autorun，测试改为手动或由 xtask 注入命令 |
| 4 | `starry uboot` 进入 `opi#` 后失败 | 原厂 U-Boot 输出 `No ethernet found`，无法使用 TFTP；同时没有 `loady` | 放弃该板当前固件下的 TFTP 路径，改为 Linux SSH 写 eMMC bootfs |
| 5 | 串口命令变成 `3etenv`、`4ftp`、`oad` | CH340 + 原生 Ubuntu 在 1500000 下双向传输不稳定 | 缩短命令、重复发送；长期应换串口驱动链路或 USB-TTL |
| 6 | StarryOS 成功启动但 22 端口拒绝连接 | 默认 `init.sh` 没有启动 SSH 服务 | 增加条件启动 `sshd` 的逻辑 |
| 7 | `sshd` 接受连接后立即断开 | OpenSSH 预认证沙箱调用未实现的 seccomp，随后发生堆破坏 | 确认为内核/进程内存问题，不再继续调整 SSH 配置 |
| 8 | 尝试 Dropbear 替代 OpenSSH | 监听端口可建立，但握手后同样断开 | 说明问题不只在 OpenSSH seccomp，更可能涉及 `fork`/地址空间/堆内存语义 |

## 3. 启动卡死的根因

### 3.1 表现

内核输出物理内存区域后不再继续：

```text
Found physical memory regions:
...
```

最初怀疑 SMP、驱动 probe 或串口输出，但真正问题是 RK3588 设备树包含大量设备 MMIO 区域，超过平台层固定容量。

### 3.2 修复

涉及文件：

- `platforms/axplat-dyn/src/mem.rs`
  - `MMIO_REGION_CAPACITY` 从 `16` 增加到 `128`。
  - 增加容量回归测试。
- `os/arceos/modules/axhal/src/mem.rs`
  - `MAX_REGIONS` 从 `128` 增加到 `256`。

修复后 StarryOS 可以继续完成驱动初始化、挂载伪文件系统并进入用户态。

## 4. 禁止开机自动运行测试

正常交互镜像启动后不应自动进入测试模式。

本轮调整：

- `os/StarryOS/starryos/src/init.sh`
  - 不再自动执行 `starry-run-case-tests`。
  - 若存在 `/test_runner.sh`，仍保留 Visual CI 专用启动钩子。
- `scripts/axbuild/src/starry/test/assets.rs`
  - `autorun_profile_script` 设为 `None`。
- `scripts/axbuild/src/starry/test/tests/qemu_discovery_tests.rs`
  - 测试改为断言 grouped cases 不安装 profile autorun。

手动测试应由用户显式运行，或由以下命令通过 `shell_init_cmd` 驱动：

```bash
cargo xtask starry test qemu
```

## 5. 当前可靠的 eMMC 启动方案

### 5.1 为什么不再使用 TFTP

该板原厂 U-Boot 实测输出：

```text
Net:   No ethernet found.
```

并且：

- 没有可用的 RTL8125 U-Boot 网卡驱动；
- 没有 `loady`；
- 串口发送长命令容易丢字。

因此：

```bash
cargo xtask starry uboot \
  --uboot-config os/StarryOS/configs/board/orangepi-5-plus-uboot.toml
```

在当前 U-Boot 固件上不能作为稳定部署方式。

### 5.2 实际采用的方案

1. 启动 Orange Pi Linux。
2. 通过 Linux SSH 上传 `image.fit`。
3. 将 FIT 写入 `/boot/image.fit`。
4. 保留 `/boot/boot.scr.linux.bak`，便于恢复 Linux。
5. `/boot/boot.scr` 从 eMMC 加载 FIT：

```text
setenv fit_addr_r 0x5480000
load mmc 1:1 ${fit_addr_r} image.fit
bootm ${fit_addr_r}#config-ostool
```

部署辅助脚本：

```bash
./starryos.bash fit
```

注意：执行前必须确认 `image.fit` 确实包含最新的 `starryos.bin`，不能只看 `cargo xtask starry build` 成功。可检查：

```bash
ls -lh --time-style=long-iso \
  target/aarch64-unknown-linux-musl/release/starryos.bin \
  target/aarch64-unknown-linux-musl/release/image.fit

dumpimage -l target/aarch64-unknown-linux-musl/release/image.fit
```

若 `starryos.bin` 时间已更新但 `image.fit` 仍是旧文件，不应直接部署旧 FIT。

## 6. 从 StarryOS 恢复 Linux

Linux 启动脚本备份在 bootfs：

```text
/boot/boot.scr.linux.bak
```

在 U-Boot `opi#` 下执行：

```text
load mmc 1:1 0x5480000 boot.scr.linux.bak
source 0x5480000
```

成功后可看到：

```text
Welcome to Orange Pi 1.2.0 Focal
IP: 192.168.6.133
orangepi@orangepi5plus:~$
```

Linux SSH：

```bash
ssh orangepi@192.168.6.133
```

默认密码：

```text
orangepi
```

由于 1500000 串口可能破坏首字符，出现 `oad`、`3etenv` 等情况时应重新发送完整命令，不能对错误回显继续执行 `source`。

## 7. 串口乱码

### 7.1 本轮证据

- 波特率为 1500000 时仍能间歇看到完整的 `opi#` 和 `root@starry:/root #`，因此不是简单的 115200/1500000 配错。
- Python `pyserial`、picocom 均出现乱码，说明不是单一终端软件问题。
- 同一转接器以前在 WSL 下正常，说明接线和板端 UART 大概率正常，差异主要在宿主机 USB 串口驱动链路。
- 乱码不仅影响显示，还会破坏 PC → 板端命令，例如 `setenv` 变成 `3etenv`。

### 7.2 Ubuntu 侧排查

```bash
sudo systemctl disable --now ModemManager
sudo fuser -k /dev/ttyUSB0
echo -1 | sudo tee /sys/module/usbcore/parameters/autosuspend
sudo stty -F /dev/ttyUSB0 1500000 cs8 -cstopb -parenb \
  -ixon -ixoff -crtscts raw
sudo picocom -b 1500000 --flow n --parity n --databits 8 /dev/ttyUSB0
```

`fuser -k` 没有输出表示当时没有进程占用串口，属于正常情况。`setserial` 未安装或驱动不支持 `low_latency` 时可以跳过。

若仍乱码，优先尝试：

1. 缩短 TX/RX/GND 线并确保共地；
2. 换主机 USB 口；
3. 换支持 1500000 的 USB-TTL；
4. 恢复此前稳定的 Windows/WSL USB 串口驱动链路。

调试 UART 使用 3.3V，只接 GND、TX、RX，不接 VCC。

### 7.3 Type-C 不能替代当前控制台

Orange Pi 5 Plus 的 USB Device Type-C 口可用于 MaskROM，但 StarryOS 当前没有完整的 USB Gadget/CDC ACM 控制台实现。因此不能直接用一根 Type-C 数据线替代调试 UART。

若要实现该能力，需要补充：

- RK3588 USB Device Controller 支持；
- USB Gadget 框架；
- CDC ACM 串口或 ECM/RNDIS 网络设备；
- 对应的控制台或网络接入层。

## 8. SSH 调试结论

### 8.1 已完成的配置

- 主机密钥：

```text
~/.ssh/starry_orangepi
~/.ssh/starry_orangepi.pub
```

- 计划连接命令：

```bash
ssh -i ~/.ssh/starry_orangepi root@192.168.6.133
```

- `init.sh` 已尝试启动 `/usr/sbin/sshd`。

### 8.2 OpenSSH 失败证据

`sshd -D -d` 日志：

```text
ssh_sandbox_child: prctl(PR_SET_SECCOMP): Invalid argument [preauth]
SSH2_MSG_KEXINIT sent [preauth]
free(): invalid next size (fast)
```

连接在密钥交换阶段关闭，尚未进入用户公钥认证。因此以下操作不能修复该问题：

- 重写 `authorized_keys`；
- 修改 root 密码；
- 更换客户端密钥；
- 强制 `aes128-ctr` 或其他 KEX/cipher；
- 反复重启 `sshd`。

### 8.3 Dropbear 试验

Ubuntu Focal rootfs 中安装了：

```text
dropbear-bin 2019.78-2build1
```

Dropbear 能短暂监听 TCP 端口，但客户端连接后仍立即关闭。这说明不能把根因仅归结为 OpenSSH seccomp；更可能存在进程派生、地址空间复制、用户态堆或相关 syscall 语义问题。

Dropbear 自动启动方案未通过验证，因此没有作为最终源码修复保留。

## 9. 下一步修复建议

SSH 的正确修复方向是内核，而不是继续替换 SSH 配置：

1. 编写最小 `fork`/`clone` + `malloc/free` 回归程序。
2. 分别验证父进程和子进程在写时复制后的堆元数据。
3. 验证 `execve`、文件描述符继承和 signal 行为。
4. 明确 `prctl(PR_SET_SECCOMP)` 应返回的错误及 OpenSSH 兼容路径。
5. 在最小回归通过后再运行：

```bash
sshd -D -d
```

6. 最后验证完整 SSH：

```bash
ssh -i ~/.ssh/starry_orangepi root@192.168.6.133 \
  'echo STARRY_SSH_OK; uname -a; id'
```

验收标准：

- SSH 握手不再触发 `free(): invalid next size`；
- 公钥认证成功；
- 能稳定执行远程命令；
- 断开一个会话后 SSH 服务仍继续监听；
- 串口控制台仍可正常使用。

## 10. 关键文件

| 路径 | 用途 |
|---|---|
| `os/StarryOS/configs/board/orangepi-5-plus.toml` | StarryOS 实板构建配置 |
| `os/StarryOS/configs/board/orangepi-5-plus-uboot.toml` | U-Boot runner 配置 |
| `os/StarryOS/starryos/src/init.sh` | 用户态初始化与 SSH 启动 |
| `platforms/axplat-dyn/src/mem.rs` | 动态平台 MMIO 区域收集 |
| `os/arceos/modules/axhal/src/mem.rs` | HAL 内存区域汇总 |
| `scripts/axbuild/src/starry/test/assets.rs` | Starry 测试资源和 autorun 配置 |
| `starryos.bash` | Linux SSH → eMMC bootfs 部署脚本 |
| `scripts/orangepi-uboot-recover-linux.py` | 从 U-Boot 恢复 Linux 的串口脚本 |
| `scripts/orangepi-uboot-boot-starry.py` | 从 eMMC 加载 FIT 的串口脚本 |

## 11. 结论

本轮已经证明：

1. RK3588 StarryOS 的早期启动卡死来自 MMIO/内存区域容量不足。
2. 原厂 U-Boot 缺少可用以太网驱动时，eMMC FIT 部署比 TFTP 更可靠。
3. 串口乱码和 SSH 失败是两个独立问题。
4. 当前系统已经具备控制台和网络，不应再次回退到刷整盘镜像排查。
5. SSH 的剩余阻塞点是 StarryOS 进程/内存语义，需要最小回归测试和内核修复。
