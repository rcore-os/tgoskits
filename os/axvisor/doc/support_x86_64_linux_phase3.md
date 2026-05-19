# Axvisor x86_64 Linux 支持：第 3 阶段文档

本文档记录 `support_x86_64_linux_phases.md` 中“阶段 3：boot_params / zero page 构造”的实现结果、验证情况和下一阶段入口。

## 阶段目标

第 3 阶段目标是在 x86 Linux direct boot 分支中构造 Linux boot protocol 需要的 `boot_params` / zero page。

本阶段仍不真正启动 Linux，不设置 Linux 专用 boot stub，也不切换 vCPU 初始寄存器状态。当前 loader 完成 bzImage payload、initramfs 和 `boot_params` 加载后仍返回 `Unsupported`，提示阶段 4 继续处理 Linux boot stub。

## 已完成产物

- `os/axvisor/src/vmm/images/x86/boot_params.rs`
  - 新增 `BootParamsBuilder`，集中封装 Linux zero page 字段写入。
  - 从 bzImage 拷贝 setup header 保留区到 `boot_params.hdr`。
  - 修补 direct boot 所需字段：
    - `sentinel`
    - `type_of_loader`
    - `loadflags`
    - `heap_end_ptr`
    - `code32_start`
    - `setup_data`
    - `cmd_line_ptr` / `ext_cmd_line_ptr`
    - `ramdisk_image` / `ramdisk_size`
    - `ext_ramdisk_image` / `ext_ramdisk_size`
  - 在 `boot_params` 内写入空 command line，占位地址位于 zero page 内部。
  - 构造第一版 E820 memory map：
    - guest 主 RAM 按可用 RAM / reserved range 拆分。
    - `boot_params` 和 boot stub 低地址区域标记为 reserved。
    - 传统 `0xa0000..0x100000` 区域标记为 reserved。
    - VM config 中的 passthrough device range 标记为 reserved。
    - VM config 中的 passthrough address range 标记为 reserved。
  - 新增错误类型 `BootParamsError`，覆盖 setup header 截断、地址溢出、布局错误和 E820 entry 过多。
  - 新增单元测试，覆盖 header patch、initrd 字段、E820 低地址 reserved、passthrough reserved 和截断镜像错误。
- `os/axvisor/src/vmm/images/mod.rs`
  - x86_64 下新增 `x86::boot_params` 模块接入。
  - Linux memory image 和 fs image 分支在加载 kernel payload/initramfs 前构造并写入真实 `boot_params`。
  - E820 reserved range 从当前 VM 的 `passthrough_devices` 和 `passthrough_addresses` 收集。
  - 阶段提示更新为：payload、initramfs 和 boot_params 已加载，Linux boot stub 从阶段 4 开始。

## 当前 boot_params 策略

当前 `boot_params` 位于固定 GPA `0x7000`，大小 `0x1000`。Linux boot stub 预留区位于 `0x8000..0x9000`。

command line 暂时不从 VM config 写入。builder 在 zero page 内写入一个空字符串，并设置有效的 `cmd_line_ptr`，这样字段形态满足 boot protocol，实际 `console`、`rdinit` 等参数仍按阶段计划优先通过内核内建 command line 或镜像构建流程解决。

E820 采用保守策略：只把明确的主 RAM 空洞暴露为 RAM，低地址启动元数据、传统保留区和 passthrough MMIO 都写成 reserved，避免 Linux 早期内存管理把设备或 loader 元数据当成普通内存使用。

## 当前行为

合法 Linux bzImage：

1. ImageLoader 自动识别 bzImage。
2. 解析 setup header 并计算 protected-mode payload offset。
3. 根据 VM config 和 header 构造 `X86LinuxLoadLayout`。
4. 构造 `boot_params`，写入 setup header、initrd 信息、空 command line 和 E820。
5. 将 `boot_params` 加载到 `0x7000`。
6. 将 boot stub 预留页加载到 `0x8000`。
7. 加载 kernel payload 和 initramfs。
8. 返回 `Unsupported`，提示阶段 4 继续实现 Linux boot stub。

非 Linux x86 image：

1. bzImage header 解析失败。
2. loader 继续原有 kernel load、ramdisk load、BIOS stub load 和 Multiboot info patch 流程。

## 验证情况

已完成验证：

```bash
cargo fmt --all -- --check
```

结果：通过。

```bash
cargo xtask axvisor build \
  --config os/axvisor/configs/board/qemu-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/nimbos-x86_64-qemu-smp1.toml
```

结果：x86_64 Axvisor release build 通过，无 warning。

```bash
AX_CONFIG_PATH=/code/tgoskits/tmp/axbuild/axconfig/axvisor/x86_64-unknown-none/.axconfig.toml \
AX_TARGET=x86_64-unknown-none \
AXVISOR_VM_CONFIGS=/code/tgoskits/os/axvisor/configs/vms/nimbos-x86_64-qemu-smp1.toml \
AX_LOG=info \
AX_PLATFORM=x86-qemu-q35 \
AX_IP=10.0.2.15 \
AX_ARCH=x86_64 \
AX_GW=10.0.2.2 \
cargo clippy -p axvisor \
  --target x86_64-unknown-none \
  -Z unstable-options \
  --target-dir /code/tgoskits/target \
  --features ax-std/myplat,ept-level-4,fs,vmx,log/release_max_level_info \
  --config 'target.x86_64-unknown-none.rustflags=["-Clink-arg=-Tlinker.x","-Clink-arg=-no-pie","-Clink-arg=-znostart-stop-gc"]' \
  --bin axvisor \
  --release \
  -- -D warnings
```

结果：target-specific x86_64 Axvisor clippy 通过。

## 遗留风险和下一阶段入口

- 当前 boot stub 仍是零填充预留页，Linux 还不能从该路径进入内核。
- `boot_params` 的 command line 目前为空；如后续 early console 仍不可见，优先调整 Linux 镜像内建 command line 或 initramfs。
- E820 目前使用主 RAM + reserved range 的保守模型，尚未引入 ACPI、MPTable 或完整 PC firmware tables。
- 第 4 阶段应新增 Linux 专用 x86 boot stub，并让 vCPU 初始入口把 `boot_params` 地址传给 Linux boot protocol。
