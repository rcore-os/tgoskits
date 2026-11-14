# 龙芯 3A6000 的 Axvisor Linux 板卡用例

## 1. 运行边界

本用例通过板卡服务的 `LoongSon-PC` 和原生 UEFI axloader 启动 Axvisor，
再运行一个 Linux 7.1 客户机。宿主使用单核，客户机使用一个 vCPU、独立 RAM、
BusyBox initramfs、虚拟串口与独立的只读 VirtIO Block 镜像，不使用板卡原系统
分区。启动和资源所有权见[设计文档](../../../../design/axvisor-loongson-3a6000-linux.md)。

### 1.1 宿主控制台

宿主命令行由 `smoke/board-loongson-3a6000-ll.toml` 完整提供，
`earlycon=ns16550,mmio,0x1fe001e0` 负责早期输出，
`console=uart8250,mmio,0x1fe001e0` 选择已注册的运行时串口，
`axvisor.vm_start=1` 选择客户机。地址参数不创建设备资源，也不重设线路或波特率；
IRQ 仍须来自真实 ACPI 描述和平台控制器。

已检查的固件没有 SPCR，但 DSDT 提供 COMA，MADT 提供其 LIOPIC 拓扑。
只有 earlycon 输出或 `runtime console active` 不代表中断输入可用。
客户机 `[kernel].cmdline` 的 `console=ttyS0,115200` 属于虚拟串口，
不能拿来替代宿主选择。

### 1.2 客户机资源

`resources/prepare.py` 固定 Linux 源码提交
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，在独立源码副本和输出目录构建。
源码树中的用户改动与既有构建不会被清理。`manifest.json` 绑定源码身份、配置、
工具链和输出摘要；缺少源码或工具时明确失败，不回退到其他 Linux 版本。

`resources/prepare-board.py` 复用该准备步骤，再通过
`cargo xtask image pack-initramfs` 把块夹具装入宿主基底归档。
运行时后端为 `/guest/loongson-block.img`，不是构建机路径；内核、BusyBox initrd
和 VM 配置随后由既有 builtin bundle 附加。块镜像仅包含测试文件
`/axvisor-board-fixture.txt`，内容为 `AXVISOR_LOONGSON_BLOCK_FIXTURE_V1`。

## 2. 准备与验证

推荐直接使用正式板卡入口，由 `smoke/guest-builds.toml` 的 `host_prepare`
准备资源，再构建和运行宿主。构建机需要项目固定的 Rust 工具链、LoongArch
编译工具、Linux 构建工具以及 ext4 镜像工具；板卡服务应已配置且目标可租用。

### 2.1 执行入口

将 `AXVISOR_LINUX_SOURCE` 指向上述 Linux 提交所在的源码树。构建产物保存在
`target/axbuild/loongson-3a6000/`，输入身份不变时复用经过校验的缓存。

```bash
export AXVISOR_LINUX_SOURCE=/path/to/linux-7.1
cargo xtask board ls
cargo xtask axvisor test board --board loongson-3a6000-ll --list
AXBUILD_NO_SNAPSHOT=1 cargo xtask axvisor test board \
  --board loongson-3a6000-ll --test-case smoke
```

如果只需提前准备资源，可在同一环境运行
`python3 -B test-suit/axvisor/normal/board-loongson-3a6000-ll/resources/prepare-board.py --jobs 8`。
不要把缺少文件、准备失败或资源缓存命中当作客户机运行成功。

### 2.2 成功判定

正式步骤先等待宿主管理提示符，再发送 `vm console 1` 接入客户机；
只有随后执行的命令产生成功行才计入结果。用例检查实际 Linux 7.1 与
LoongArch 架构、确定性计算、RAM 文件写读和定时等待，以及 `/dev/vda` 的只读
状态、ext4 挂载和预置内容，任何 panic、失败标记或超时都应使外层任务失败。

完整交付还要求同一最终版本第二次独立板级启动，并记录两轮上传镜像摘要、
完整日志和板卡租约释放。当前开发记录在
[#2798](https://github.com/rcore-os/tgoskits/issues/2798) 和
[草稿 PR #2799](https://github.com/rcore-os/tgoskits/pull/2799)；
其中的局部静态检查、Hello 或诊断采集结果不能替代这两轮 Linux 验收。
