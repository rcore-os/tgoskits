---
sidebar_position: 5
sidebar_label: "ArceOS"
---

# ArceOS 测试

ArceOS 的测试覆盖两类用例：**Rust 用例**（每个用例是一个完整的 Cargo 项目，包含自己的 `Cargo.toml`、`build-{target}.toml` 和 `qemu-{arch}.toml`）和 **C 用例**（通过 Makefile 构建的 C 语言程序，由 `test_cmd` 文件定义测试序列）。两类用例的发现和处理方式有所不同，但最终都通过 QEMU 运行并使用正则匹配判定结果。

## 命令

```text
cargo xtask arceos test qemu --arch <arch> [--test-group <group>] [--test-case <case>] [--package <pkg>]
```

ArceOS 测试命令支持通过 `--test-group` 选择测试组（`rust`、`c` 或自定义组），通过 `--test-case` 过滤特定用例，通过 `--package` 指定特定 Rust 包。不指定 `--test-group` 时默认运行所有组。

## 测试组

| 组 | 路径 | 说明 |
|----|------|------|
| `rust` | `test-suit/arceos/rust/` | Rust 包测试 |
| `c` | `test-suit/arceos/c/` | C 语言测试 |
| 自定义 | `test-suit/arceos/<group>/` | 通过 `--test-group` 选择 |

Rust 组和 C 组是预定义的标准组，分别用于验证 ArceOS 的 Rust 应用和 C 语言兼容性。自定义组允许开发者按需添加新的测试类别。

## Rust 用例

通过 `discover_qemu_cases()` 发现，每个 Rust 包是标准 Cargo 项目。用例结构：

```text
test-suit/arceos/rust/<package>/
├── Cargo.toml
├── src/main.rs
├── build-{target}.toml
└── qemu-{arch}.toml
```

执行流程：
1. `discover_qemu_cases()` 扫描 `test-suit/arceos/rust/`
2. 从 `Cargo.toml` 读取 `package` 名
3. `ensure_package_runtime_assets()` 准备运行时资产（如 FAT32 disk.img）
4. 按 build config 分组 → 每组构建一次
5. 逐 case 加载 QEMU 配置 → `AppContext::run_qemu()` → 正则判定

Rust 用例的每个目录既是 Cargo 项目（有自己的 `Cargo.toml`），也是 axbuild 的 build wrapper（有 `build-{target}.toml`）和测试用例（有 `qemu-{arch}.toml`）。这种三位一体的结构使得每个 Rust 包可以独立定义自己的构建配置（features、环境变量）和运行配置（QEMU 参数、超时、正则）。

`ensure_package_runtime_assets()` 为需要磁盘镜像等运行时资产的用例（如文件系统测试）预生成必要的文件。例如 FAT32 disk.img 会在首次运行时创建并缓存，后续运行直接复用。

## C 用例

通过 `test_cmd` 文件定义调用序列：

```bash
# test_cmd 格式
test_one "MAKE_VARS" "expect_output_file"
```

每个 C 用例目录可包含：
- `.c` 源文件
- `axbuild.mk`：Makefile 片段
- `features.txt`：Cargo features
- `test_cmd`：测试调用定义
- `expect_*.out`：预期输出

执行流程：
1. 解析 `features.txt` 和 `test_cmd`
2. 设置交叉编译环境（`make defconfig`）
3. `make build` 编译
4. `make justrun` 运行（QEMU 内）
5. 输出与 `expect_*.out` 比对

C 用例的测试方式与 Rust 用例有本质区别：Rust 用例通过 axbuild 的标准发现和分组流程执行，而 C 用例使用独立的 Makefile 构建系统。`test_cmd` 文件定义了多轮编译-运行-比对序列，每轮通过 `test_one` 函数指定编译参数（`MAKE_VARS`）和预期输出文件（`expect_output`）。`features.txt` 中的 features 会被注入到 ArceOS 内核的编译配置中，允许 C 测试启用特定的内核功能。
