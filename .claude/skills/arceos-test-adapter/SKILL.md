---
name: arceos-test-adapter
description: 适配 ArceOS 测试用例到 xtask 测试框架。当用户提到 ArceOS 测试、适配测试用例、让测试通过 cargo xtask test arceos、添加新的 ArceOS task 测试，或者需要修改 test-suit/arceos/task 下的测试时，使用此技能。无论用户是否明确使用"适配"这个词，只要涉及到 ArceOS 测试框架的配置或测试用例调整，就应该使用此技能。
---

# ArceOS 测试适配指南

此技能用于将 ArceOS 测试用例适配到 `cargo xtask test arceos` 测试框架中。

## 适配检查清单

### 1. 目录结构对比

对比目标测试目录与现有工作测试目录（推荐参考 `wait_queue`）：

```
test-suit/arceos/task/
├── <test-name>/
│   ├── Cargo.toml              # 需要检查
│   ├── src/
│   │   └── main.rs           # 需要检查
│   ├── .axconfig.toml          # 必须：平台配置
│   ├── .qemu.toml            # 必须：QEMU 默认配置
│   ├── qemu-aarch64.toml       # 必须：aarch64 QEMU 参数
│   ├── qemu-riscv64.toml       # 必须：riscv64 QEMU 参数
│   ├── qemu-x86_64.toml        # 必须：x86_64 QEMU 参数
│   └── qemu-loongarch64.toml   # 必须：loongarch64 QEMU 参数
```

### 2. Cargo.toml 适配

检查 `Cargo.toml` 中的 `edition` 字段：

```toml
# 错误：硬编码版本
edition = "2021"

# 正确：使用 workspace 配置
edition.workspace = true
```

完整示例：
```toml
[package]
name = "test-name"
version = "0.1.0"
edition.workspace = true
authors = ["Your Name <email@example.com>"]
description = "Test description"

[dependencies]
axstd = { workspace = true, features = ["multitask", "irq"], optional = true }
```

### 3. 配置文件准备

从参考目录复制配置文件到目标目录：

```bash
# 假设 wait_queue 是参考目录
cp test-suit/arceos/task/wait_queue/.axconfig.toml test-suit/arceos/task/<test-name>/
cp test-suit/arceos/task/wait_queue/.qemu.toml test-suit/arceos/task/<test-name>/
cp test-suit/arceos/task/wait_queue/qemu-*.toml test-suit/arceos/task/<test-name>/
```

**配置文件说明：**
- `.axconfig.toml` - ArceOS 平台硬件配置（MMIO、中断、内存映射等）
- `.qemu.toml` - 默认 QEMU 启动参数
- `qemu-{arch}.toml` - 各架构特定的 QEMU 参数

### 4. 源代码兼容性检查

检查 `src/main.rs` 中的属性语法：

```rust
// 旧语法（Rust 较早版本）
#[cfg_attr(feature = "axstd", no_mangle)]
fn main() { ... }

// 新语法（当前 Rust 版本需要 unsafe 包装）
#[cfg_attr(feature = "axstd", unsafe(no_mangle))]
fn main() { ... }
```

**搜索并替换：**
```bash
grep -n "no_mangle" test-suit/arceos/task/<test-name>/src/
```

如果找到 `no_mangle` 且没有 `unsafe` 包装，需要修改。

### 5. 清理旧测试文件

删除旧测试系统遗留的文件：

```bash
cd test-suit/arceos/task/<test-name>/
rm -f *.out *.bin *.elf test_cmd
```

### 6. 代码格式化

在测试验证前，先格式化代码：

```bash
cargo fmt -p <crate-name>
```

**例如：**
```bash
# 对于 arceos-irq 测试
cargo fmt -p arceos-irq
```

### 7. 运行测试验证

```bash
cargo xtask test arceos
```

**期望输出：**
- 编译成功，无错误、无警告
- QEMU 正常启动
- 测试用例执行完成，输出 "ok: <test-name>"

**检查警告：** 如果编译时出现警告，需要修复所有警告后再提交

## 常见问题修复

### 问题 1: edition 不匹配

**症状：** 警告或不一致的构建行为

**修复：**
```toml
# Cargo.toml 中
edition.workspace = true
```

### 问题 2: unsafe 属性错误

**症状：** 编译错误 `unsafe attribute used without unsafe`

**修复：**
```rust
#[cfg_attr(feature = "axstd", unsafe(no_mangle))]
```

### 问题 3: 缺少配置文件

**症状：** xtask 报告找不到配置文件

**修复：** 从参考测试目录复制完整的 `.toml` 文件集

### 问题 4: 架构不完整

**症状：** 某个架构测试失败

**检查：** 确保所有架构的 `qemu-{arch}.toml` 文件都存在：
- qemu-aarch64.toml
- qemu-riscv64.toml
- qemu-x86_64.toml
- qemu-loongarch64.toml

### 问题 5: 编译警告

**症状：** 编译成功但有警告信息

**修复：** 根据警告内容修复代码，常见警告包括：
- 未使用的导入/变量 → 删除未使用的代码
- 死代码 → 移除不会执行的代码
- 变量命名不规范 → 改为 snake_case

**检查方法：**
```bash
cargo build -p <crate-name> --release 2>&1 | grep -i warning
```

### 问题 6: 代码格式不一致

**症状：** 代码风格与项目标准不一致

**修复：** 运行代码格式化工具：
```bash
cargo fmt -p <crate-name>
```

## 适配新增测试用例步骤

当需要新增全新的测试用例时：

1. **创建目录结构**
   ```bash
   mkdir -p test-suit/arceos/task/<new-test>/src
   ```

2. **复制模板配置**
   ```bash
   cp test-suit/arceos/task/wait_queue/*.toml test-suit/arceos/task/<new-test>/
   ```

3. **创建 Cargo.toml**
   ```toml
   [package]
   name = "test-new-name"
   version = "0.1.0"
   edition.workspace = true
   description = "Test description"

   [dependencies]
   axstd = { workspace = true, features = ["multitask"], optional = true }
   ```

4. **编写测试代码**
   - 在 `src/main.rs` 中实现测试逻辑
   - 使用 `#[cfg_attr(feature = "axstd", unsafe(no_mangle))]` 标记 main 函数

5. **格式化代码**
   ```bash
   cargo fmt -p test-new-name
   ```

6. **验证**
   ```bash
   cargo xtask test arceos
   ```

## 参考示例

### wait_queue（参考模板）
```
test-suit/arceos/task/wait_queue/
├── Cargo.toml                    # edition.workspace = true
├── src/
│   └── main.rs                   # unsafe(no_mangle)
├── .axconfig.toml
├── .qemu.toml
├── qemu-aarch64.toml
├── qemu-riscv64.toml
├── qemu-x86_64.toml
└── qemu-loongarch64.toml
```

### 适配后的 irq
```
test-suit/arceos/task/irq/
├── Cargo.toml                    # 修改：edition.workspace = true
├── src/
│   ├── main.rs                   # 修改：unsafe(no_mangle)
│   └── irq.rs
├── .axconfig.toml               # 新增
├── .qemu.toml                  # 新增
├── qemu-aarch64.toml            # 新增
├── qemu-riscv64.toml            # 新增
├── qemu-x86_64.toml             # 新增
├── qemu-loongarch64.toml         # 新增
```

## xtask 测试工作原理

`cargo xtask test arceos` 的工作流程：

1. **扫描测试目录** - 查找 `test-suit/arceos/task/` 下所有包含配置文件的目录
2. **交叉编译** - 为每个目标架构编译测试用例
3. **QEMU 运行** - 使用对应架构的 `qemu-{arch}.toml` 参数启动模拟器
4. **结果验证** - 检查程序是否正常退出并输出期望的结果
