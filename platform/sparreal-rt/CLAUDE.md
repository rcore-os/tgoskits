[根目录](../../CLAUDE.md) > [platform](../) > **sparreal-rt**

# Sparreal RT - 平台运行时

## 模块职责

Sparreal RT (Sparreal Runtime) 是平台特定的运行时实现，负责将通用的内核代码与具体的硬件平台连接起来，提供系统启动和平台接口实现。

## 入口与启动

### 主要入口点
- **`lib.rs`**: 运行时库入口，重导出内核接口
- **启动函数**: `main()` - 使用 `#[somehal::entry]` 宏标记的平台主函数

### 启动流程
```rust
#[somehal::entry]
fn main() -> ! {
    somehal::println!("Starting Sparreal OS kernel...");
    sparreal_kernel::hal::setup::start_kernel()
}
```

启动过程：
1. 打印启动信息
2. 调用内核启动函数 `start_kernel()`
3. 内核完成初始化后不会返回（`-> !`）

## 对外接口

### 重导出的接口
- **内核接口**: 通过 `pub use sparreal_kernel::*` 重导出所有内核接口
- **入口宏**: 通过 `pub use sparreal_kernel::entry` 提供应用入口宏
- **平台实现**: 通过 `hal_impl.rs` 实现具体的平台接口

### 平台实现模块
- **`hal_impl.rs`**: 实现平台特定的 HAL 接口

## 关键依赖与配置

### 核心依赖
```toml
[dependencies]
somehal = { workspace = true }     # 硬件抽象层
sparreal-kernel = { workspace = true }  # 内核核心
log = {workspace = true}           # 日志接口
```

### 特性配置
- **`hv`**: 虚拟化支持（传递给 somehal）
- **`uspace`**: 用户空间支持（传递给 somehal，依赖 mmu）

## 数据模型

### 平台接口实现
实现 `sparreal_kernel::platform_if::Platform` trait，提供：
- 中断等待机制
- 系统关闭功能
- 其他平台特定的操作

### 构建配置
- **链接脚本**: `link.ld` - 定义内存布局和符号
- **构建脚本**: `build.rs` - 处理构建时的配置

## 测试与质量

### 当前测试状态
- ❌ **单元测试**: 缺少平台特定的单元测试
- ✅ **集成测试**: 通过系统启动进行集成验证
- ⚠️ **硬件测试**: 需要在目标硬件上进行验证

### 建议的测试策略
1. **启动测试**: 验证在各种硬件配置下的正常启动
2. **接口测试**: 测试平台接口实现的正确性
3. **错误处理**: 测试硬件异常和错误处理路径
4. **性能测试**: 测量平台接口的调用开销

### 质量工具
- **交叉编译**: 确保在目标平台上正确编译
- **链接验证**: 验证链接脚本的正确性
- **启动日志**: 通过启动日志验证初始化过程

## 常见问题 (FAQ)

### Q: 如何适配新的硬件平台？
A: 创建新的 platform 目录，实现 Platform trait，并更新构建配置。

### Q: 启动失败如何调试？
A: 检查启动日志，确认硬件初始化、内存映射等关键步骤是否正确。

### Q: 如何添加新的平台特性？
A: 在 Cargo.toml 中添加特性标志，并在实现中根据特性进行条件编译。

### Q: 链接脚本如何配置？
A: 根据目标平台的内存布局修改 `link.ld`，确保代码和数据正确放置。

## 相关文件清单

### 核心文件
- `src/lib.rs` - 运行时库入口
- `src/hal_impl.rs` - 平台接口实现

### 构建相关
- `build.rs` - 构建脚本
- `link.ld` - 链接器脚本
- `Cargo.toml` - 项目配置

### 配置文件
- 各种平台特定的配置文件（可能在父目录中）

---

## 变更记录 (Changelog)

### 2025-12-03 09:30:10
- 初始化 sparreal-rt 平台文档
- 完成启动流程和接口分析
- 识别平台适配要点
- 建立测试策略和常见问题解答