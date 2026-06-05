# StarryOS 可加载内核模块（kmod）：编译与使用

本文档说明 StarryOS 示例内核模块 `os/StarryOS/modules/hello` 的编译与加载流程。

`.ko` 加载器（`kmod-loader`）与 `cargo xtask starry kmod build` 子命令由 PR #851
提供；本仓在其之上提供最小可加载模块示例 `hello` 与端到端加载测例。

## 1. 模块一览

| 模块 | 作用 | 形态 |
|------|------|------|
| `hello` | 最小 LKM 示例：`init` 打印问候，并构造一个 `Vec` 演示从模块内做堆分配 + `{:?}` Debug 格式化 | 可加载 `.ko` |

## 2. 编译为 `.ko`

```bash
# 编译全部模块（产物在 target/<arch>/kmod/）
cargo xtask starry kmod build --all --arch x86_64

# 仅编译单个模块
cargo xtask starry kmod build --module os/StarryOS/modules/hello --arch x86_64
```

产物：`target/x86_64/kmod/hello.ko`。

### 构建约束（为何不是普通 `cargo build`）

`.ko` 不自带任何依赖：它的每一个未定义符号都由内核加载器在装载时对内核
`.kallsyms` 决议。因此模块必须与内核“同构”地编译：

- **内核需以 `STARRY_KMOD=y` 构建**：`lto=false` + `-C relocation-model=static
  -C code-model=large`，使内核把模块要重定位的每个 `core`/`alloc`/`starry_kernel`
  符号（包括 `core::fmt` 的 Debug builders 与 Rust 分配器 shim，乃至
  `__rust_no_alloc_shim_is_unstable_v2` 这类守卫）都保留进 `.kallsyms`，而不是被
  内联/DCE 掉。正因如此，`hello` 才能直接使用 `Vec`（堆分配）与 `{:?}`（Debug）。
- **模块与内核同一份 no-pie target spec + `-Z build-std=core,alloc`**：保证
  `core`/`alloc` 的 crate-disambiguator hash 与内核完全一致，否则模块对它们的
  重定位会因符号名（含 hash）对不上而无法在 `.kallsyms` 决议。
- **模块以内核“解析后的 feature 集合”共编**：构建子命令不再硬编码 feature，而是
  按内核构建流程派生（`ax-hal/<平台> + starryos/qemu`），与 `cargo xtask starry
  build` 一致，从而让 `starry_kernel` 及其依赖闭包被统一到同一套 feature、得到同一
  组 crate hash。
- **`-C relocation-model=static -C code-model=large`**：默认 `pic` 会让外部符号走
  GOT，产生加载器不支持的 `R_X86_64_REX_GOTPCRELX(42)`；改为 64 位绝对寻址
  `R_X86_64_64`（加载器支持，且能覆盖 `0xffff_8000_…` 高位内核地址）。纯 codegen
  flag，不改 crate-hash。

快速自检（需 llvm-tools 的 `rust-nm`/`rust-objdump`）：

```bash
KELF=target/x86_64-unknown-none/release/starryos
rust-nm -u target/x86_64/kmod/hello.ko                       # 每个 undef 符号都应能在 $KELF 中找到
rust-objdump -r target/x86_64/kmod/hello.ko | grep R_X86_64  # 应仅出现 R_X86_64_64
```

## 3. 运行期加载 `.ko`

`.ko` 不入库；测例在配置期由 `cargo xtask starry kmod build` 生成并安装进 guest
rootfs 的 `/lib/modules/`。guest 内通过 `finit_module(2)` 加载：

```c
int fd = open("/lib/modules/hello.ko", O_RDONLY | O_CLOEXEC);
syscall(SYS_finit_module, fd, "", 0);
```

端到端冒烟测例（`test-suit/starryos/normal/qemu-kmod/kmod-modules`）：

```bash
cargo xtask starry rootfs --arch x86_64
cargo xtask starry test qemu --arch x86_64 -c kmod-modules   # 期望 PASS
```

加载成功后串口可见：

```
LOADED /lib/modules/hello.ko
Hello, Kernel Module!
Vector contents: [1, 2, 3, 4, 5]
```

## 4. 架构支持

运行期加载器 `kmod-loader` 本身支持 aarch64 / riscv64 / loongarch64 / x86_64 四种
架构的重定位。但**当前 `cargo xtask starry kmod build` 仅支持 `--arch x86_64`**：

x86_64 的 StarryOS-QEMU 使用静态平台 `ax-hal/x86-pc`，因此上文“按内核解析后的
feature 集合共编”有唯一确定的平台 feature 可对齐；而 aarch64/riscv64 的
StarryOS-QEMU 走动态平台 `plat-dyn`，没有单一平台 feature 可固定，且其内核以不同的
relocation-model 编译。把其它架构接入，需要为每种架构单独适配 feature 派生与 codegen
并在 QEMU 中验证，属于独立的后续工作，而非简单地放开架构判断。
