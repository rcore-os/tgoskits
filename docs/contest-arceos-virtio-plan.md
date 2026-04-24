# ArceOS VirtIO 比赛方案

## 1. 作品主题

作品主题定为：

`ArceOS VirtIO 驱动健壮性与回归测试增强`

该主题聚焦 ArceOS 的 VirtIO 驱动包装层与接线层，围绕“避免 panic、统一错误处理、增强探测稳定性、补足自动化测试”展开。

## 2. 与比赛要求的对应关系

根据比赛 PDF，作品需要满足以下要求：

1. 有效代码不少于 `1000` 行。
2. 至少具备 `2` 个相互独立的功能模块。
3. 核心代码具备自动化测试覆盖。
4. 代码结构清晰，符合 Rust 规范。

本方案通过“功能模块 + 测试模块”的方式满足要求：

1. 模块A：VirtIO 驱动健壮性增强
2. 模块B：VirtIO 自动化测试与回归体系

## 3. 总体目标

本作品不把目标停留在“修几个 `unwrap`”，而是要形成一个完整的小型子项目：

1. 提升 VirtIO 设备初始化、探测、地址转换、错误处理路径的稳定性。
2. 为 VirtIO 驱动包装层建立系统化单元测试与回归测试。
3. 形成可提交 PR、可展示、可复现实验结果的比赛作品。

## 4. 模块拆分

### 4.1 模块A：VirtIO 驱动健壮性增强

#### 目标

将现有 VirtIO 驱动代码从“功能可跑”提升到“错误可控、边界清晰、行为稳定”。

#### 主要修改范围

1. `components/axdriver_crates/axdriver_virtio/src/lib.rs`
2. `components/axdriver_crates/axdriver_virtio/src/gpu.rs`
3. `components/axdriver_crates/axdriver_virtio/src/input.rs`
4. `components/axdriver_crates/axdriver_virtio/src/net.rs`
5. `components/axdriver_crates/axdriver_virtio/src/socket.rs`
6. `os/arceos/modules/axdriver/src/virtio.rs`

#### 核心工作内容

1. 清理或替换初始化路径中的 `unwrap`。
2. 统一 `DevResult` / `Option` 错误返回风格。
3. 增强 MMIO / PCI 探测路径的边界处理。
4. 强化 HAL 指针转换和 DMA 路径的诊断能力。
5. 增强 `net` / `socket` 等设备包装层的健壮性。
6. 改善 `input` 设备身份信息和占位实现。

#### 模块产出

1. 一组更健壮的 VirtIO 驱动包装实现。
2. 一组清晰可追踪的提交历史。
3. 一份可直接用于 PR 和比赛说明的功能增强说明。

### 4.2 模块B：VirtIO 自动化测试与回归体系

#### 目标

为模块A建立自动化验证闭环，证明改动不是“单纯清理代码”，而是“有测试支撑的工程增强”。

#### 主要修改范围

1. `components/axdriver_crates/axdriver_virtio/src/lib.rs`
2. 视情况新增 `components/axdriver_crates/axdriver_virtio/tests/`
3. 视情况接入 `test-suit/arceos/`

#### 核心工作内容

1. 为 `as_dev_type()` 补设备映射测试。
2. 为 `as_dev_err()` 补错误映射测试。
3. 为 `probe_mmio_device()` / `probe_pci_device()` 补边界测试。
4. 为 `socket` / `net` 的纯逻辑函数补单元测试。
5. 选取可行的 ArceOS QEMU 路径做集成回归。

#### 模块产出

1. 一组 `cargo test` 可执行的单元测试。
2. 一组可复用的驱动回归测试。
3. 一组可以写入比赛材料的自动化验证结果。

## 5. 推荐执行顺序

### 第一阶段：建立基线

1. 跑通 `ArceOS hello world`
2. 跑通 `ArceOS` 测试链路
3. 建立比赛分支

当前状态：

1. 已完成

### 第二阶段：模块A 最小可用版本

1. 清理 `gpu` / `input` 初始化 panic
2. 清理 `probe_mmio_device()` 空指针 panic
3. 强化 ArceOS `virtio.rs` 中 HAL 非空检查

当前状态：

1. 前两项已完成
2. 第三项进行中

### 第三阶段：模块B 第一版

1. 增加 `as_dev_type()` 单元测试
2. 增加 `probe_mmio_device()` 边界测试
3. 增加 `as_dev_err()` 覆盖测试

当前状态：

1. 前两项已完成
2. 第三项待做

### 第四阶段：扩大模块A 范围

1. 审查 `net.rs`
2. 审查 `socket.rs`
3. 改进 `input.rs` 占位实现
4. 补更完整的探测与日志逻辑

### 第五阶段：扩大模块B 范围

1. 增加 `net/socket` 逻辑测试
2. 引入一到两条 QEMU 集成回归
3. 形成稳定测试命令清单

## 6. 代码量策略

当前这条线起点明确，但现阶段代码量很小，不可能仅靠少量 panic 修复达到比赛要求。

因此需要采用“功能增强 + 测试增强”的双模块累计策略。

### 目标分配

1. 模块A：约 `500` 到 `700` 行
2. 模块B：约 `300` 到 `500` 行

### 代码量来源

1. 错误处理重构
2. 探测逻辑增强
3. HAL 诊断辅助函数
4. 设备包装层改进
5. 单元测试
6. 集成测试

## 7. 当前已完成工作

截至当前分支，已完成：

1. `gpu` 初始化路径去 panic
2. `input` 初始化路径去 panic
3. `probe_mmio_device()` 空指针安全返回
4. `as_dev_type()` 单元测试
5. `probe_mmio_device()` 空指针测试

当前已有提交：

1. `virtio: avoid panic during gpu and input init`
2. `virtio: harden mmio probe and add unit tests`

## 8. 后续最近任务

下一步优先级如下：

1. 处理 `os/arceos/modules/axdriver/src/virtio.rs` 中 HAL 非空检查
2. 为 `as_dev_err()` 增加测试
3. 审查 `net.rs` 和 `socket.rs` 的可扩展点
4. 设计下一批集成回归入口

## 9. 预期最终交付物

最终比赛作品应包含：

1. 一条独立比赛开发分支
2. 一组结构清晰的提交历史
3. 两个清晰独立的功能模块
4. 单元测试与回归测试结果
5. 一份作品说明文档
6. 一份可用于上游 PR 的改动说明
