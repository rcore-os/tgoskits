# Allocator Test Suite

本目录包含 allocator 的集成测试与压力测试。

## 结构

- `integration_test.rs`
  常规集成测试，覆盖全局分配器、Buddy、Slab 以及多模块协同行为。
- `dma32_pages_test.rs`
  低地址页分配相关测试。
- `stress_test.rs`
  长时间随机、耗尽恢复、碎片化恢复，以及真实多线程跨 CPU 压力测试。
  这些测试默认使用 `#[ignore]`，不会进入常规 `cargo test` 路径。
- `common/`
  共享测试辅助模块，提供宿主堆管理、线程本地 CPU mock、固定种子 RNG 和通用初始化逻辑。

单元测试仍位于 `src/**/*.rs` 的 `#[cfg(test)]` 模块中，文档测试位于公共 API 注释中。

## 常用命令

```bash
# 常规测试
cargo test

# 串行执行，便于排查测试交互
cargo test -- --test-threads=1

# 仅运行常规集成测试
cargo test --test integration_test

# 仅运行压力测试
cargo test --test stress_test -- --ignored --nocapture

# 串行运行压力测试，便于定位多线程问题
cargo test --test stress_test -- --ignored --nocapture --test-threads=1
```

## 设计原则

- 常规测试应保持快速、稳定，可直接进入 CI。
- 压力测试用于长时间 workload、真实多线程 cross-CPU 交互、耗尽恢复、碎片化恢复与统计不变量检查。
- 性能测量移至 `benches/`，不在测试中混入 benchmark 逻辑。

## 注意事项

1. 所有测试都使用宿主分配器申请一块测试堆。
2. 压力测试默认被忽略，需要显式运行。
3. 多线程压力测试默认被 `#[ignore]` 标记，需要显式运行。
