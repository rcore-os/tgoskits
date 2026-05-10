---
sidebar_position: 1
sidebar_label: "构建流程"
---

# 构建流程

TGOSKits 的统一构建入口是根目录 `cargo xtask`。它经由 `tg-xtask` 调用 `scripts/axbuild` 中的统一分发层，再落到各子系统的解析与执行流程。

## 调用链

```text
cargo xtask / cargo arceos / cargo starry / cargo axvisor
    |
    v  (.cargo/config.toml aliases)
cargo run -p tg-xtask -- <args>
    |
    v  (xtask/src/main.rs)
axbuild::run()
    |
    v  (scripts/axbuild/src/lib.rs)
+-- Commands::Test       -> test_std::run_std_test()
+-- Commands::Clippy     -> clippy::run_workspace_check()
+-- Commands::Board      -> board::execute()
+-- Commands::ArceOS     -> ArceOS::new()?.execute()
+-- Commands::Starry     -> Starry::new()?.execute()
+-- Commands::Axvisor    -> Axvisor::new()?.execute()
```

## 为什么使用统一入口

- 三套系统在同一 workspace 中协同演化，需要统一的命令入口
- 根命令隐藏了大量 arch/target、snapshot 和配置解析细节
- Axvisor、StarryOS 和 ArceOS 的最短命令入口保持一致风格

## 推荐实践

| 场景 | 推荐方式 | 不推荐 |
|------|---------|--------|
| 常规开发 | `cargo xtask ...` | 直接操作子系统 Makefile |
| 排查子项目细节 | 下沉到 `os/arceos/Makefile` 等 | - |
| 构建/运行/测试 | 统一用 `cargo xtask` 命令族 | 混用不同入口 |

## 相关文档

- [命令总览](./cmd) — 所有可用命令的完整列表
- [Arch / Target 映射](./arch) — `--arch` 与 `--target` 的关系
- [Snapshot 与 Build Info](./more) — 参数持久化与构建上下文
- [ostool 执行后端](./ostool) — 底层编译与镜像生成
- [构建系统完整说明](/docs/design/reference/build-system) — 详细技术文档
