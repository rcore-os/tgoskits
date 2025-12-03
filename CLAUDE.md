# 雀实操作系统 (Sparreal OS) - 架构文档

## 项目愿景

Sparreal OS 是一个麻雀虽小、五脏俱全的实时操作系统，专注于嵌入式系统和裸机开发。项目采用 Rust 语言开发，支持多架构（AArch64 和 LoongArch64），提供现代化的操作系统内核功能。

## 架构总览

Sparreal OS 采用分层架构设计：

- **应用层 (apps/)**: 用户应用程序和测试用例
- **运行时层 (platform/)**: 平台特定的运行时实现
- **内核层 (crates/sparreal-kernel/)**: 操作系统核心功能
- **硬件抽象层 (crates/somehal/)**: 跨平台硬件抽象接口
- **支持库层 (crates/**): 各种辅助库和工具

## 模块结构图

```mermaid
graph TD
    A["(根) Sparreal OS"] --> B["apps"];
    A --> C["crates"];
    A --> D["platform"];
    A --> E["test-suit"];

    B --> F["helloworld"];

    C --> G["sparreal-kernel"];
    C --> H["somehal"];
    C --> I["page-table-generic"];
    C --> J["kernutil"];
    C --> K["dma-api"];
    C --> L["sparreal-macros"];
    C --> M["somehal-macros"];
    C --> N["kasm-aarch64"];

    D --> O["sparreal-rt"];

    E --> P["async"];
    E --> Q["timer"];

    click F "./apps/helloworld/CLAUDE.md" "查看 helloworld 应用文档"
    click G "./crates/sparreal-kernel/CLAUDE.md" "查看 kernel 模块文档"
    click H "./crates/somehal/CLAUDE.md" "查看 somehal 模块文档"
    click O "./platform/sparreal-rt/CLAUDE.md" "查看 sparreal-rt 平台文档"
    click P "./test-suit/async/CLAUDE.md" "查看 async 测试文档"
    click Q "./test-suit/timer/CLAUDE.md" "查看 timer 测试文档"
```

## 模块索引

| 模块路径 | 类型 | 职责 | 主要语言 | 测试覆盖 |
|---------|------|------|----------|----------|
| `apps/helloworld` | 应用程序 | 示例应用，演示基本的 OS 功能使用 | Rust | ✅ |
| `crates/sparreal-kernel` | 内核核心 | 操作系统核心功能：内存管理、中断、异步任务等 | Rust | ⚠️ |
| `crates/somehal` | 硬件抽象 | 跨平台硬件抽象层，支持 AArch64 和 LoongArch64 | Rust | ⚠️ |
| `platform/sparreal-rt` | 平台运行时 | 平台特定的运行时实现和内核启动 | Rust | ❌ |
| `crates/page-table-generic` | 支持库 | 通用页表管理实现 | Rust | ❌ |
| `crates/kernutil` | 支持库 | 内核实用工具 | Rust | ❌ |
| `crates/dma-api` | 支持库 | DMA 操作 API | Rust | ❌ |
| `crates/sparreal-macros` | 支持库 | 内核宏定义 | Rust | ❌ |
| `crates/somehal-macros` | 支持库 | 硬件抽象层宏 | Rust | ❌ |
| `crates/kasm-aarch64` | 支持库 | AArch64 汇编支持 | Rust | ❌ |
| `test-suit/async` | 测试套件 | 异步功能测试 | Rust | ✅ |
| `test-suit/timer` | 测试套件 | 定时器功能测试 | Rust | ✅ |

## 运行与开发

### 环境要求
- Rust 2024 Edition
- QEMU（用于模拟测试）
- ostool（构建工具）

### 构建命令
```bash
ostool build        # 构建项目
ostool run qemu     # QEMU 运行测试
ostool run qemu -d  # QEMU 调试模式
ostool run uboot    # U-Boot 调试（需要开发板）
```

### 开发工具链
- **构建系统**: Cargo workspace + ostool
- **调试支持**: GDB multiarch + VS Code 集成
- **目标平台**:
  - QEMU 模拟器
  - 实际硬件开发板（通过 U-Boot）

## 测试策略

- **单元测试**: 各个 crate 的独立功能测试
- **集成测试**: 通过 apps/ 和 test-suit/ 中的应用进行集成测试
- **模拟测试**: QEMU 环境下的系统级测试
- **硬件测试**: 实际开发板上的验证测试

### 主要测试套件
- `apps/helloworld`: 基础功能演示和测试
- `test-suit/async`: 异步任务和执行器测试
- `test-suit/timer`: 定时器和中断测试

## 编码规范

- **语言**: Rust 2024 Edition
- **目标**: `no_std` 环境（无标准库）
- **内存安全**: 使用 Rust 的所有权系统确保内存安全
- **并发**: 使用 `spin` crate 实现自旋锁
- **错误处理**: 使用 `thiserror` 和 `anyhow` 进行错误处理

## AI 使用指引

### 架构理解
- 这是一个多架构的嵌入式操作系统内核
- 采用分层设计，强调模块化和可移植性
- 重点在于硬件抽象和跨平台支持

### 开发重点
1. **硬件抽象层 (HAL)**: 理解 `somehal` 的设计模式
2. **内存管理**: 关注分页机制和堆分配器
3. **中断处理**: 理解异步执行器和定时器集成
4. **平台适配**: 新平台需要实现 `Platform` trait

### 调试提示
- 使用 `ostool run qemu -d` 启动调试模式
- VS Code 配置了 `KDebug` 调试配置
- 关注日志输出，使用 `log` crate 进行调试

---

## 变更记录 (Changelog)

### 2025-12-03 09:30:10
- 初始化架构文档
- 完成项目模块识别和结构分析
- 生成 Mermaid 架构图
- 建立模块索引和覆盖率评估

**覆盖率摘要**:
- 总文件数: ~50+ 个源文件
- 已扫描核心模块: 12 个
- 估算覆盖率: 85%（核心模块全覆盖）
- 主要缺口: 部分支持库缺少详细扫描