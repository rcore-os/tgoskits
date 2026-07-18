---
sidebar_position: 3
sidebar_label: "参数与配置"
---

# 参数与配置

axbuild 将一次构建或运行拆成四类相互独立的输入：命令行参数负责本次选择，Snapshot 负责复用上次选择，Build Config 声明编译能力，QEMU/U-Boot/Board Config 声明启动方式。`context/resolve.rs` 和各系统的 `build/` 模块将这些输入解析为确定的请求和 Cargo 配置。

| 配置 | 典型位置 | 负责内容 | 不负责内容 |
| --- | --- | --- | --- |
| Snapshot | `tmp/axbuild/.arceos.toml` 等 | 最近使用的 package、arch、target、config 和运行配置路径 | Cargo feature、QEMU 参数 |
| Build Config | `tmp/axbuild/config/<package>/build-<target>.toml` | feature、环境变量、日志、CPU 数，以及系统专属字段 | firmware、CPU 型号、ELF/BIN 转换 |
| Board Build Config | `os/<system>/configs/board/*.toml` | checked-in 的 target、能力和板卡默认值 | QEMU 命令行 |
| QEMU Config | `os/<system>/configs/qemu/qemu-<arch>.toml` 或测试目录 | QEMU 参数、UEFI、`to_bin`、成功/失败判定 | Cargo feature |
| U-Boot / Board Run Config | 显式路径或 ostool 约定位置 | 下载、部署和运行协议 | 构建能力选择 |

对应源码集中在 `scripts/axbuild/src/context/`、`build/` 以及各系统的 `config.rs`、`build/`、`rootfs.rs` 中。

## 1. 请求解析

`AppContext::prepare_*_request()` 将 CLI、已有 Snapshot 和 Build Config 中可作为选择器的字段合并为 `ResolvedBuildRequest`、`ResolvedStarryRequest` 或 `ResolvedAxvisorRequest`。解析在真正调用 Cargo 前完成，随后用户命令以 `SnapshotPersistence::Store` 写回快照；测试流程使用 `Discard`，不会污染开发者的最近选择。

### 1.1 架构映射

`context/arch.rs` 只接受以下四组固定映射：

| `arch` | 裸机逻辑 target | 默认 managed rootfs | musl 工具链前缀 |
| --- | --- | --- | --- |
| `aarch64` | `aarch64-unknown-none-softfloat` | `rootfs-aarch64-alpine.img` | `aarch64-linux-musl` |
| `x86_64` | `x86_64-unknown-none` | `rootfs-x86_64-alpine.img` | `x86_64-linux-musl` |
| `riscv64` | `riscv64gc-unknown-none-elf` | `rootfs-riscv64-alpine.img` | `riscv64-linux-musl` |
| `loongarch64` | `loongarch64-unknown-none-softfloat` | `rootfs-loongarch64-alpine.img` | `loongarch64-linux-musl` |

### 1.2 默认选择

只传 `--arch` 时查表补齐 target，只传 `--target` 时反向补齐 arch；两者都传时必须严格匹配。不同系统未给出架构选择器时使用下表中的默认值。

| 系统 | 默认 arch | 默认 target |
| --- | --- | --- |
| ArceOS | `aarch64` | `aarch64-unknown-none-softfloat` |
| StarryOS | `riscv64` | `riscv64gc-unknown-none-elf` |
| Axvisor | `aarch64` | `aarch64-unknown-none-softfloat` |

逻辑 target 是用户和配置使用的裸机 target。采用 `ax-std` 的 Rust 构建会在内部映射到 `scripts/targets/std/pie/<arch>-unknown-linux-musl.json`，例如 `aarch64-unknown-none-softfloat` 映射为 `aarch64-unknown-linux-musl` 的 PIE JSON target。`AX_TARGET` 仍保存原始裸机 target，供内核构建上下文读取。

## 2. 状态复用

### 2.1 状态文件

Snapshot 将最近成功解析的选择器写入系统独立文件；它保存请求状态，而不保存 Cargo feature 或 QEMU 参数本体。

| 系统 | 文件 | 系统专属字段 |
| --- | --- | --- |
| ArceOS | `tmp/axbuild/.arceos.toml` | `package` |
| StarryOS | `tmp/axbuild/.starry.toml` | 固定 package `starryos`，不持久化 package |
| Axvisor | `tmp/axbuild/.axvisor.toml` | `vmconfigs` |

三者共同保存 `arch`、`target`、`smp`、`config`，并分别在 `[qemu]`、`[uboot]` 下保存运行配置路径。

### 2.2 合并规则

Snapshot 不是无条件的“CLI 缺省值”。为避免把旧 target 的配置带入新 target，源码设置了交叉抑制和整体继承条件：

- 显式 `--arch` 时不继承旧 `target`；显式 `--target` 或 Build Config 已给出 target 时不继承旧 `arch`。
- 只有没有切换 package、arch、target 或 config 等选择器时，才继承旧的 Build Config 路径和 QEMU/U-Boot 路径。
- ArceOS 的 package 优先级为 CLI → Build Config → Snapshot；若三者均无值，命令报错。含 `app-c` 的配置强制选择 `ax-libc`，与其他 package 选择冲突时直接报错。
- StarryOS 的 `--smp`、Axvisor 的 `--smp` 均优先于 Snapshot；Axvisor 显式 `--vmconfigs` 优先于 Snapshot 和 Build Config。
- `defconfig` 会更新 arch、target 和 config，并清除旧 QEMU/U-Boot 配置路径；Axvisor 保留已有 Snapshot 中的 `vmconfigs`。

例如：

```toml
# tmp/axbuild/.arceos.toml
package = "arceos-httpserver"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
config = "tmp/axbuild/config/arceos-httpserver/build-aarch64-unknown-none-softfloat.toml"

[qemu]
qemu_config = "os/arceos/configs/qemu/qemu-aarch64.toml"
```

该快照只展示可复用的选择器。下一次请求是否继承它仍由 CLI 是否显式切换 package、arch、target 或 config 决定。

`AXBUILD_NO_SNAPSHOT=1` 只禁止本次写回，不禁止读取已有 Snapshot。需要完全排除历史状态时，应显式给出关键选择器或删除对应 `.toml` 文件。

## 3. 构建配置

共享的 `BuildInfo` 结构只有四个字段：

```toml
features = ["fs", "net", "ax-driver/virtio-blk"]
log = "Warn"
max_cpu_num = 4

[env]
BACKTRACE = "y"
DWARF = "y"
```

| 字段 | 默认值 | 源码行为 |
| --- | --- | --- |
| `features` | `[]` | 显式 Cargo 能力列表；排序、去重并按 `ax-std` 的公开 feature 边界转发 |
| `log` | `Warn` | 转成 `AX_LOG=<小写级别>` |
| `max_cpu_num` | 无 | 写入 `SMP`；大于 1 时显式增加 `smp` 能力；0 非法 |
| `[env]` | 空表 | 原样加入 Cargo 环境；部分键会驱动工具链选项 |

默认路径由 `default_build_info_path_in_workspace()` 生成：

```text
tmp/axbuild/config/<package>/build-<logical-target>.toml
```

其中 package 分别是 ArceOS app 名、`starryos` 和 `axvisor`。`--config <PATH>` 可覆盖默认路径。

### 3.1 系统配置形态

三个系统共享 `BuildInfo`，但 board 文件提供的选择器和缺失默认配置的处理方式不同。下表对应各自的 `config.rs` 与 `build/load.rs`，用于判断某个字段应放入何处。

| 系统 | 额外字段 | 缺失默认配置时 |
| --- | --- | --- |
| ArceOS | board 文件可含 `package`、`target`；C app 配置可含 `app-c` | `build`/`qemu` 优先复制 target 和 package 匹配的 `qemu-*` board；否则生成空能力配置 |
| StarryOS | board 文件含 `target`，可有同名 `.its` | `build`/`qemu` 必须找到 target 对应的默认 `qemu-*` board 并复制；`.its` 一并复制 |
| Axvisor | board 文件含 `target`、`vm_configs` | 读取配置时优先复制 target 匹配的默认 board；找不到才生成空能力配置 |

`cargo xtask <system> defconfig <board>` 是显式选择板卡的入口；`config ls` 列出源码树中可以选择的名称。

### 3.2 特性校验

`BuildInfo::validate_features()` 在 Cargo 配置生成前检查所有 Build Config 和 `FEATURES` 输入。验证规则将 feature 归属限制在当前 Cargo 接口，并保证各系统只接收自己的配置字段：

- `axstd` 与 `axstd/<feature>` 不属于有效 feature 名称；配置应使用 package 实际声明的 Cargo feature。
- `dyn-plat`、`plat-dyn`、`axplat-dyn`、`ax-hal/plat-dyn`、`ax-std/plat-dyn`、`axvm/plat-dyn`、`ax-driver/plat-dyn` 不属于有效 BuildInfo feature。
- Build Config 根字段只接受 `env`、`features`、`log`、`max_cpu_num` 和系统专属字段；`std`、`plat_dyn` 会在 TOML 读取时报告结构错误。
- `app-c` 只由 ArceOS 的 C app 路径消费；StarryOS 和 Axvisor 的配置读取器会拒绝该字段。
- Axvisor 的 `reject_unsupported_nested_platform_features()` 额外检查 `axplat-dyn/*`、`ax-std/<platform>` 和裸平台名。

`FEATURES` 提供以逗号或空白分隔的外部 feature 输入；`apply_makefile_features()` 对每项执行相同验证、规范化和去重，然后并入 BuildInfo。

### 3.3 标准库构建

ArceOS Rust app、StarryOS 和 Axvisor 共用 `into_prepared_base_cargo_config_with_metadata()`：

1. 验证 `max_cpu_num` 和 feature。
2. 将 `ax-std/foo` 规范成逻辑能力 `foo`，保留 `ax-hal/*`、`ax-driver/*`、`ax-runtime/*` 边界。
3. 根据应用 package 和 `ax-std` 的 Cargo metadata，仅把实际存在的 feature 分发到应用或 `ax-std`。
4. 将裸机 target 映射到 musl PIE JSON target，并启用 `build-std = ["std", "panic_abort"]`。
5. 生成 `tmp/axbuild/std-libs/` 下的占位库和 linker wrapper，避免宿主静态库污染内核链接。
6. 设置对应的 musl `CC_*`、`AR_*`、`CFLAGS_*` 和 bindgen 参数；release profile 使用 `panic = "abort"` 且关闭 LTO。

Build Config 中 `[env]` 的以下键会补充 Rust 工具链选项：

| 条件 | 追加 rustflags |
| --- | --- |
| `DWARF=y/yes/1/true/on` | `-Cdebuginfo=2 -Cstrip=none -Cforce-frame-pointers=yes` |
| `BACKTRACE=y/yes/1/true/on` | `-Cforce-frame-pointers=yes` |
| feature 含 `stack-protector` | `-Zstack-protector=strong` |

## 4. 产物与启动

Cargo 基础配置以 `to_bin = false` 创建：`cargo xtask <system> build` 输出 ELF，运行器根据选中的启动配置准备实际加载的产物格式。

需要 QEMU 启动时，`to_bin` 必须由所选 QEMU TOML 显式给出：

```toml
args = ["-machine", "virt"]
uefi = false
to_bin = true
```

- `to_bin = true`：运行前由 ostool 从 ELF 准备 raw BIN。
- `to_bin = false`：运行器直接使用 ELF。
- `uefi = true`：Axvisor 强制要求同时显式设置 `to_bin = true`，否则在启动前报错。仓库内 ArceOS、StarryOS 的 x86_64/loongarch64 默认 UEFI 配置也遵循该组合。
- QEMU machine、CPU、加速、firmware、UEFI 和设备参数由当前 TOML 提供；`apply_smp_qemu_arg()` 仅维护用户请求的 `-smp` 值。

测试或 app case 若配置了全局 `-snapshot`，UEFI 路径会将其改写为各 `-drive` 的 `snapshot=on`。这样系统盘保持写时复制，同时 EFI pflash/ESP 不会被全局 snapshot 语义错误地变成不可写状态。

## 5. 虚拟化后端

x86_64 的虚拟化后端属于 Build Config 的显式能力：Intel 配置声明 `vmx`，AMD 配置声明 `svm`。`patch_axvisor_cargo_config()` 将已解析的 feature 与 VM 配置写入 Cargo 环境；QEMU CPU flags 由测试或运行 TOML 声明，例如 VMX 用例使用 `+vmx-ept`，SVM 用例使用 `+svm,+npt,+nrip-save`。

仓库中的参考配置包括：

```text
test-suit/axvisor/normal/qemu/build-x86_64-unknown-none-vmx.toml
test-suit/axvisor/normal/qemu/build-x86_64-unknown-none-svm.toml
os/axvisor/configs/board/qemu-x86_64.toml
```

## 6. 环境变量

环境变量只用于兼容入口和运行环境覆盖，不应替代 Build Config 或启动 TOML。下表列出 axbuild 直接读取的外部变量及其职责。

| 变量 | 作用 |
| --- | --- |
| `FEATURES` | 追加经验证的 Build Config feature，逗号或空白分隔 |
| `AXBUILD_NO_SNAPSHOT` | 禁止本次写回 Snapshot，不影响读取 |
| `AXBUILD_QEMU_SYSTEM_LOONGARCH64` | 指定 LoongArch LVZ QEMU 可执行文件 |
| `AXBUILD_QEMU_DIR` | 指定 LoongArch LVZ QEMU 所在目录 |
| `AXBUILD_TEST_TIMEOUT_SCALE` | 按整数倍放大测试 QEMU timeout |
| `STARRY_APK_REGION` | Starry managed rootfs 的 APK 区域，支持 `china`/`cn`、`us`/`usa` |
| `TGOS_IMAGE_LOCAL_STORAGE` | 覆盖 image storage 目录 |
| `TGOS_IMAGE_REGISTRY_FALLBACK_URL` | 覆盖镜像注册表 fallback URL |
| `AXBUILD_KEEP_QEMU_LOG` | 保留 QEMU 日志，便于事后符号化 |

`AX_LOG`、`SMP`、`AX_TARGET`、`AX_ARCH` 和 `AXVISOR_VM_CONFIGS` 主要由 axbuild 根据上述配置生成，不建议用外部环境绕过请求解析。
