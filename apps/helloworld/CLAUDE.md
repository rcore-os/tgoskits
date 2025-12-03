[根目录](../../CLAUDE.md) > [apps](../) > **helloworld**

# HelloWorld - 示例应用

## 模块职责

HelloWorld 是 Sparreal OS 的示例应用程序，演示了基本的操作系统功能使用，包括日志输出、定时器和异步任务的使用。

## 入口与启动

### 主要入口点
- **`src/main.rs`**: 应用程序主函数
- **入口宏**: `#[sparreal_rt::entry]` - 标记应用入口点

### 启动流程
```rust
#[sparreal_rt::entry]
fn main() {
    info!("Hello, world!");

    // 设置定时器测试
    one_shot_after(Duration::from_millis(200), || {
        TEST_IRQ.store(true, core::sync::atomic::Ordering::SeqCst);
    }).unwrap();

    // 等待定时器中断
    loop {
        if TEST_IRQ.load(core::sync::atomic::Ordering::SeqCst) {
            break;
        }
    }

    println!("All tests passed!");
}
```

## 对外接口

### 依赖的运行时接口
- **日志接口**: `log::info!`, `println!` - 输出日志信息
- **定时器接口**: `sparreal_rt::os::time::one_shot_after` - 一次性定时器
- **异步支持**: 使用 `extern crate sparreal_rt` 导入运行时

## 关键依赖与配置

### 核心依赖
```toml
[dependencies]
log = {workspace = true}          # 日志接口
sparreal-rt = {workspace = true}  # Sparreal 运行时
```

### 编译配置
- **`no_main`**: 不使用标准的 main 函数
- **`no_std`**: 在非标准库环境下编译（非 Windows/Unix）

## 数据模型

### 测试状态管理
- **`TEST_IRQ`**: `AtomicBool` - 用于测试定时器中断触发状态
- **超时机制**: 200ms 后触发中断，验证异步定时器功能

### 同步机制
- **原子操作**: 使用 `AtomicBool` 和 `Ordering::SeqCst` 确保线程安全
- **忙等待循环**: 简单的事件等待机制

## 测试与质量

### 当前测试状态
- ✅ **功能测试**: 验证基本的日志和定时器功能
- ✅ **集成测试**: 在 QEMU 环境中运行完整测试
- ✅ **异步测试**: 验证定时器回调的正确执行

### 测试覆盖
- **日志系统**: 验证 `log` crate 和 `println!` 的正确工作
- **定时器系统**: 测试异步定时器调度和执行
- **原子操作**: 验证多线程安全的状态管理

### 建议的扩展测试
1. **多定时器测试**: 同时调度多个定时器
2. **长时间运行测试**: 验证系统稳定性
3. **错误处理测试**: 测试定时器调度失败的情况
4. **性能测试**: 测量定时器精度和开销

## 常见问题 (FAQ)

### Q: 如何添加新的测试用例？
A: 在 main 函数中添加新的测试逻辑，使用相应的日志和同步机制。

### Q: 定时器精度如何？
A: 定时器精度依赖于底层硬件定时器，通常在毫秒级别。

### Q: 如何调试应用启动问题？
A: 检查日志输出，确认运行时环境和依赖是否正确配置。

### Q: 如何使用异步功能？
A: 通过 `sparreal_rt` 提供的异步 API，如定时器、任务调度等。

## 相关文件清单

### 源代码
- `src/main.rs` - 应用主程序

### 构建配置
- `Cargo.toml` - 项目配置
- `build.rs` - 构建脚本
- `.qemu.toml` - QEMU 运行配置
- `qemu-aarch64.toml` - AArch64 QEMU 配置
- `qemu-la64.toml` - LoongArch64 QEMU 配置

### 测试输出
应用运行时会输出：
```
Hello, world!
Waiting for timer interrupt...
All tests passed!
```

---

## 变更记录 (Changelog)

### 2025-12-03 09:30:10
- 初始化 helloworld 应用文档
- 完成应用功能和测试分析
- 识别扩展测试建议
- 建立常见问题解答