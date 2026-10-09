---
sidebar_position: 2
sidebar_label: "构建"
---

# Axvisor 构建

`cargo xtask axvisor build` 构建固定 package `axvisor`，随后准备客户机资源归档。内核只依赖 `BuildInfo` 中的驱动、CPU 数及功能配置；`vm_configs` 和 `--vmconfig` 是宿主 initramfs 的打包输入，客户机镜像不再编入内核。

## 1. 内核构建

`prepare_axvisor_request()` 选择架构、target 和 Build Config，`load_cargo_config()` 生成 Cargo 请求。改变客户机配置或镜像不会改变 Cargo 参数及环境。

### 1.1 配置装载

默认配置为 `tmp/axbuild/config/axvisor/build-<target>.toml`。`load_build_config()` 接受纯 `BuildInfo` 或含 `target`、`vm_configs` 的 board 配置；CLI `--smp` 覆盖 CPU 数。缺失配置优先从 `os/axvisor/configs/board/qemu-*.toml` 生成，否则生成空功能配置。

文件系统是 Axvisor 的必需能力，无需 `fs` feature。块设备驱动仍由配置显式选择；只用 initramfs 启动客户机不需要块设备驱动。x86 虚拟化后端由运行时 CPUID 选择，QEMU TOML 控制暴露 VMX 或 SVM 扩展。

### 1.2 Cargo 装配

`load_cargo_config()` 固定 package/binary 为 `axvisor`，写入 `AX_ARCH` 和原始裸机 `AX_TARGET`，合并 `FEATURES` 后排序去重并验证平台边界。裸机 target 映射到工作区 musl PIE target，链接方式与交叉 C 环境沿用共享 `BuildInfo`。

直接构建产出 ELF；QEMU 读取实际 TOML 后按 `to_bin` 决定是否转换为 BIN。VM 配置不会写入 Cargo 环境，也不会作为 `os/axvisor/build.rs` 的输入。

## 2. 自带资源打包

`load_vmconfigs()` 与 `bundle::attach()` 在内核构建之外解析客户机配置、准备镜像并复用现有 newc 打包器。运行器将归档交给 QEMU、FIT 或板卡 HTTP Boot。

### 2.1 打包输入

CLI 的 `--vmconfig <PATH>` 可重复，`--vmconfigs` 为兼容别名；非空 CLI 列表覆盖 Build Config 的 `vm_configs`。测试用例可在自己的 `host-initramfs.toml` 中指定列表，与共享 Build Config 分离。

`kernel_path`、`dtb_path`、`bios_path`、`uefi_firmware_path`、`ramdisk_path` 可引用构建机文件，支持 `${workspace}`、`${workspaceFolder}`、`${package}`、`${tmpDir}`、`${env:NAME}`；相对路径按 VM TOML 所在目录解释。板卡自行构建的资源使用 `${env:AXVISOR_GUEST_ASSETS}` 指定本地资产目录。板卡启动也允许保留 rootfs 已提供的绝对资源路径；可取得的本地文件仍优先复制到自带包。客户机可写磁盘路径维持原值。

### 2.2 归档发布

`bundle::attach()` 校验镜像非空，将启动镜像放入 `/guest/builtin/images`，生成路径已重写的 `/guest/builtin/configs`。内容哈希用于共享同一镜像；配置在内存根和磁盘根中使用相同路径。已有宿主归档保留，通过串接 newc 添加客户机资源；打包失败保留之前的输出。

构建命令也生成宿主归档。QEMU 用例会分别输出内核和归档的 SHA-256，便于核对复用的内核及实际资源。

## 3. 命令示例

以下命令使用默认构建、board 配置和显式 VM 输入。运行客户机还需按 [运行](./runtime) 选择宿主启动入口。

```bash
cargo xtask axvisor build --arch aarch64
cargo xtask axvisor build --config os/axvisor/configs/board/qemu-x86_64.toml
cargo xtask axvisor build --arch x86_64 \
  --vmconfig os/axvisor/configs/vms/qemu/x86_64/linux-smp1.toml
```
