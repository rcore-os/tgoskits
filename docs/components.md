# 组件开发指南

本指南介绍如何在 TGOSKits 中开发、测试和维护可复用的操作系统组件。

## 📋 目录

- [组件概述](#组件概述)
- [组件分类](#组件分类)
- [创建新组件](#创建新组件)
- [组件开发规范](#组件开发规范)
- [测试组件](#测试组件)
- [文档规范](#文档规范)
- [发布流程](#发布流程)

## 组件概述

TGOSKits 包含 60+ 个可复用的操作系统组件，这些组件：

- **独立可测试**: 每个组件都可以独立测试
- **高度模块化**: 组件之间松耦合，通过接口通信
- **跨项目共享**: 可以在 ArceOS、StarryOS、Axvisor 等项目中复用
- **版本管理**: 通过 Git Subtree 管理独立仓库

## 组件分类

### Hypervisor 组件

虚拟化相关组件，支持 ARM/RISC-V/x86 架构：

| 组件 | 说明 | 仓库 |
|------|------|------|
| **arm_vcpu** | ARM 虚拟 CPU | [GitHub](https://github.com/arceos-hypervisor/arm_vcpu) |
| **arm_vgic** | ARM 虚拟中断控制器 | [GitHub](https://github.com/arceos-hypervisor/arm_vgic) |
| **riscv_vcpu** | RISC-V 虚拟 CPU | [GitHub](https://github.com/arceos-hypervisor/riscv_vcpu) |
| **x86_vcpu** | x86 虚拟 CPU | [GitHub](https://github.com/arceos-hypervisor/x86_vcpu) |
| **axvm** | 虚拟机管理 | [GitHub](https://github.com/arceos-hypervisor/axvm) |
| **axvcpu** | vCPU 抽象 | [GitHub](https://github.com/arceos-hypervisor/axvcpu) |
| **axvisor_api** | Hypervisor API | [GitHub](https://github.com/arceos-hypervisor/axvisor_api) |

### ArceOS 组件

ArceOS 框架核心组件：

| 组件 | 说明 | 仓库 |
|------|------|------|
| **axcpu** | CPU 抽象 | [GitHub](https://github.com/arceos-org/axcpu) |
| **axsched** | 调度器 | [GitHub](https://github.com/arceos-org/axsched) |
| **axerrno** | 错误处理 | [GitHub](https://github.com/arceos-org/axerrno) |
| **axio** | I/O 抽象 | [GitHub](https://github.com/arceos-org/axio) |
| **percpu** | Per-CPU 变量 | [GitHub](https://github.com/arceos-org/percpu) |
| **kspin** | 自旋锁 | [GitHub](https://github.com/arceos-org/kspin) |
| **lazyinit** | 延迟初始化 | [GitHub](https://github.com/arceos-org/lazyinit) |
| **axdriver_crates** | 驱动框架 | [GitHub](https://github.com/arceos-org/axdriver_crates) |
| **axplat_crates** | 平台抽象 | [GitHub](https://github.com/arceos-org/axplat_crates) |

### Starry 组件

StarryOS 专用组件：

| 组件 | 说明 | 仓库 |
|------|------|------|
| **starry-process** | 进程管理 | [GitHub](https://github.com/Starry-OS/starry-process) |
| **starry-signal** | 信号机制 | [GitHub](https://github.com/Starry-OS/starry-signal) |
| **starry-vm** | 虚拟内存 | [GitHub](https://github.com/Starry-OS/starry-vm) |
| **axpoll** | I/O 多路复用 | [GitHub](https://github.com/Starry-OS/axpoll) |
| **rsext4** | ext4 文件系统 | [GitHub](https://github.com/Starry-OS/rsext4) |

### 基础组件

通用基础组件：

| 组件 | 说明 | 仓库 |
|------|------|------|
| **axallocator** | 内存分配器 | [GitHub](https://github.com/arceos-org/allocator) |
| **axerrno** | 错误处理 | [GitHub](https://github.com/arceos-org/axerrno) |
| **page_table_multiarch** | 多架构页表 | [GitHub](https://github.com/arceos-org/page_table_multiarch) |
| **crate_interface** | 接口抽象 | [GitHub](https://github.com/arceos-org/crate_interface) |
| **bitmap-allocator** | 位图分配器 | [GitHub](https://github.com/rcore-os/bitmap-allocator) |

## 创建新组件

### 1. 规划组件

在创建新组件前，考虑：

- **功能范围**: 组件应该单一职责
- **依赖关系**: 最小化依赖
- **接口设计**: 清晰的 API 边界
- **复用性**: 是否可以跨项目使用

### 2. 创建组件目录

```bash
# 在 TGOSKits 根目录
cd /path/to/tgoskits

# 创建组件目录
mkdir -p components/my_component/src
cd components/my_component
```

### 3. 创建 Cargo.toml

```toml
[package]
name = "my_component"
version = "0.1.0"
edition = "2021"
authors = ["Your Name <your@email.com>"]
description = "A brief description of your component"
license = "Apache-2.0 OR MIT"
repository = "https://github.com/your-org/my_component"
keywords = ["arceos", "no_std", "os"]
categories = ["os", "no-std"]

[dependencies]
# 添加必要的依赖

[features]
default = []

# 可选功能
std = []

[dev-dependencies]
# 测试依赖
```

### 4. 创建源代码

```rust
// src/lib.rs
#![no_std]

/// My component's public API
pub struct MyComponent {
    // 内部状态
}

impl MyComponent {
    /// 创建新实例
    pub fn new() -> Self {
        Self {
            // 初始化
        }
    }
    
    /// 执行操作
    pub fn do_something(&self) -> Result<(), MyError> {
        // 实现
        Ok(())
    }
}

impl Default for MyComponent {
    fn default() -> Self {
        Self::new()
    }
}

/// 错误类型
#[derive(Debug)]
pub enum MyError {
    InvalidInput,
    InternalError,
}
```

### 5. 添加到工作空间

```toml
# 在根 Cargo.toml 的 [workspace.members] 中添加
[workspace]
members = [
    # ...
    "components/my_component",
]

# 添加 patch
[patch.crates-io]
my_component = { path = "components/my_component" }
```

### 6. 添加到 repos.csv

```csv
# 在 scripts/repo/repos.csv 中添加
https://github.com/your-org/my_component,,components/my_component,ArceOS,My component description
```

## 组件开发规范

### 代码规范

1. **命名规范**

```rust
// 类型名: 大驼峰
pub struct MyStruct {}
pub enum MyEnum {}

// 函数/变量: 蛇形命名
pub fn my_function() {}
let my_variable = 0;

// 常量: 大写蛇形
pub const MAX_SIZE: usize = 1024;

// 静态变量: 蛇形命名
pub static GLOBAL_COUNTER: AtomicUsize = AtomicUsize::new(0);
```

2. **文档注释**

```rust
/// 创建新的组件实例
/// 
/// # Arguments
/// 
/// * `config` - 配置参数
/// 
/// # Returns
/// 
/// 返回新创建的实例，如果失败则返回错误
/// 
/// # Examples
/// 
/// ```
/// use my_component::MyComponent;
/// 
/// let component = MyComponent::new(config)?;
/// ```
pub fn new(config: Config) -> Result<Self, Error> {
    // 实现
}
```

3. **错误处理**

```rust
use axerrno::AxResult;

pub fn my_function() -> AxResult<()> {
    // 使用 AxResult 统一错误类型
    let value = some_operation().map_err(|e| {
        ax_err_type!(InvalidInput, "invalid input parameter")
    })?;
    
    Ok(())
}
```

4. **特性开关**

```rust
// 使用 features 控制功能
#[cfg(feature = "std")]
pub fn std_only_function() {
    // 仅在 std 环境下可用
}

#[cfg(not(feature = "std"))]
pub fn no_std_function() {
    // no_std 环境下的实现
}
```

### 接口设计

1. **提供清晰的接口**

```rust
// 好的设计: 简单清晰的 API
pub trait MyTrait {
    fn init(&mut self) -> Result<(), Error>;
    fn process(&self, data: &[u8]) -> Result<Vec<u8>, Error>;
    fn cleanup(&mut self);
}

// 避免过度复杂的接口
```

2. **使用 Builder 模式**

```rust
pub struct MyComponentBuilder {
    config: Config,
}

impl MyComponentBuilder {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
        }
    }
    
    pub fn with_option(mut self, value: bool) -> Self {
        self.config.option = value;
        self
    }
    
    pub fn build(self) -> Result<MyComponent, Error> {
        Ok(MyComponent {
            config: self.config,
        })
    }
}
```

3. **提供默认实现**

```rust
impl Default for MyComponent {
    fn default() -> Self {
        Self::new()
    }
}
```

### 性能考虑

1. **避免不必要的分配**

```rust
// 使用引用而不是克隆
pub fn process(&self, data: &[u8]) -> Result<&[u8], Error> {
    // 优先使用引用
    Ok(data)
}

// 如果需要所有权，明确说明
pub fn take_ownership(&self, data: Vec<u8>) -> Result<(), Error> {
    // 处理并获取所有权
    Ok(())
}
```

2. **使用适当的同步原语**

```rust
use kspin::SpinLock;

pub struct SharedState {
    lock: SpinLock<InnerState>,
}

impl SharedState {
    pub fn access<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut InnerState) -> R,
    {
        let mut state = self.lock.lock();
        f(&mut state)
    }
}
```

## 测试组件

### 单元测试

```rust
// src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_new() {
        let component = MyComponent::new();
        assert!(component.is_ok());
    }
    
    #[test]
    fn test_do_something() {
        let component = MyComponent::new().unwrap();
        let result = component.do_something();
        assert!(result.is_ok());
    }
}
```

运行测试：

```bash
# 在组件目录
cd components/my_component
cargo test

# 在 TGOSKits 根目录
cargo test -p my_component
```

### 集成测试

```rust
// tests/integration_test.rs
use my_component::MyComponent;

#[test]
fn test_integration() {
    let component = MyComponent::new().unwrap();
    
    // 测试完整的使用场景
    let result = component.do_something();
    assert!(result.is_ok());
}
```

### no_std 测试

```rust
// 对于 no_std 组件，使用自定义测试框架
#![no_std]
#![no_main]

#[no_mangle]
fn main() {
    // 测试代码
    test_my_component();
    println!("All tests passed!");
}

fn test_my_component() {
    let component = MyComponent::new();
    assert!(component.is_ok());
}
```

## 文档规范

### README.md 结构

```markdown
# Component Name

Brief description of the component.

## Features

- Feature 1
- Feature 2

## Usage

Add to Cargo.toml:

\`\`\`toml
[dependencies]
my_component = "0.1.0"
\`\`\`

## Example

\`\`\`rust
use my_component::MyComponent;

let component = MyComponent::new()?;
component.do_something()?;
\`\`\`

## License

Apache-2.0 OR MIT
```

### API 文档

```rust
//! # My Component
//! 
//! This component provides...
//! 
//! ## Features
//! 
//! - Feature 1: description
//! - Feature 2: description
//! 
//! ## Quick Start
//! 
//! ```rust
//! use my_component::MyComponent;
//! 
//! let component = MyComponent::new()?;
//! ```

/// Represents a component instance
/// 
/// This struct manages...
pub struct MyComponent {
    // ...
}
```

## 发布流程

### 1. 准备发布

```bash
# 1. 确保所有测试通过
cargo test

# 2. 检查文档
cargo doc --open

# 3. 更新版本号
# 编辑 Cargo.toml 中的 version

# 4. 更新 CHANGELOG.md
```

### 2. 创建 Git 标签

```bash
# 在组件独立仓库
git tag v0.1.0
git push origin v0.1.0
```

### 3. 发布到 crates.io

```bash
# 登录
cargo login

# 发布
cargo publish
```

### 4. 更新 TGOSKits

```bash
# 更新 repos.csv 中的版本标签
vim scripts/repo/repos.csv

# 拉取更新
python3 scripts/repo/repo.py pull my_component
```

## 最佳实践

### 1. 保持组件独立

- 最小化外部依赖
- 避免循环依赖
- 提供清晰的接口

### 2. 版本兼容性

- 使用语义化版本
- 保持向后兼容
- 提供迁移指南

### 3. 性能考虑

- 避免不必要的内存分配
- 使用合适的同步原语
- 提供性能基准测试

### 4. 文档完善

- 提供使用示例
- 解释设计决策
- 维护更新日志

## 常见问题

### Q: 如何处理跨组件依赖？

**A:** 通过接口抽象：

```rust
// 定义接口 trait
pub trait MyInterface {
    fn do_something(&self) -> Result<(), Error>;
}

// 组件依赖接口而不是具体实现
pub struct MyComponent<T: MyInterface> {
    interface: T,
}
```

### Q: 如何调试组件？

**A:** 使用日志：

```rust
use axlog::debug;

pub fn my_function() {
    debug!("my_function called");
    // 实现
}
```

### Q: 如何处理平台差异？

**A:** 使用 cfg 条件编译：

```rust
#[cfg(target_arch = "aarch64")]
fn arch_specific() {
    // ARM64 实现
}

#[cfg(target_arch = "riscv64")]
fn arch_specific() {
    // RISC-V 实现
}
```

## 参考资源

- [Rust API 指南](https://rust-lang.github.io/api-guidelines/)
- [Rust 嵌入式开发书籍](https://doc.rust-lang.org/stable/embedded-book/)
- [操作系统开发最佳实践](https://wiki.osdev.org/)

---

**相关文档**:
- [快速开始指南](quick-start.md)
- [构建系统说明](build-system.md)
- [仓库管理指南](repo.md)
