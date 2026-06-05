# Benchmark 使用说明

本目录包含基于 `divan` 的 benchmark 套件，围绕当前 crate 的三层结构组织：

- `buddy_allocator.rs`
  `BuddyAllocator` 的页分配、对齐分配、碎片恢复、随机页 workload
- `slab_allocator.rs`
  `SlabAllocator` 的 size class alloc/free、hot reuse、mixed-size batch、steady-state recycle
- `global_allocator.rs`
  `GlobalAllocator` 的小对象、大对象、页接口、混合 workload、cross-CPU free cycle

共享的 host-side harness 放在 `common.rs`，负责：

- region / metadata 分配
- buddy / slab / global 初始化
- 固定随机种子
- mock EII 接口

## 运行方式

```bash
# 仅检查 benchmark 是否可编译
cargo check --benches

# 运行全部 benchmark
cargo bench

# 单独运行某个 suite
cargo bench --bench buddy_allocator
cargo bench --bench slab_allocator
cargo bench --bench global_allocator
```

## 设计原则

- 使用 `divan::Bencher` 和 `divan::black_box`
- 不再保留旧的 `criterion` 代码路径
- benchmark 只使用当前公开 API，不依赖历史类型名
- 尽量让每次迭代在闭环内恢复 allocator 状态，减少跨迭代污染
- workload 使用固定模式或固定随机种子，便于复现

## CI 策略

CI 仅执行 `cargo check --benches`，确保 benchmark 工程持续可编译。
真实性能测量和结果对比保留在本地执行。
