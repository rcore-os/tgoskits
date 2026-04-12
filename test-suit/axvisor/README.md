# AxVisor Guest Cases

## 使用方式

```bash
cd tgoskits

# 运行单个测例
cargo axvisor test cases --case test-suit/axvisor/example/pass-report

# 运行一个测例集
cargo axvisor test cases --suite test-suit/axvisor/suites/examples.toml

# 显式指定架构（默认为 aarch64）
cargo axvisor test cases --arch aarch64 --suite test-suit/axvisor/suites/examples.toml

# 控制是否实时打印 guest 串口输出
cargo axvisor test cases --case test-suit/axvisor/example/pass-report --guest-log true
```

说明：

- `--case` 与 `--suite` 互斥，必须二选一。
- 不显式指定 `--arch` 时默认使用 `aarch64`。
- 单 case 默认显示 guest 串口输出；suite 默认不显示。
- runner 输出位于 `os/axvisor/tmp/cases/<run-id>/`。

## 测例设计规范

一个能够被这套流程运行的 guest 测例，目录最小形态如下：

```text
test-suit/axvisor/<category>/<case>/
├── Cargo.toml
├── case.toml
├── build-<target>.toml        # 可选
├── src/
│   └── main.rs
└── vm/
    └── <arch>.toml.in
```

各文件作用：

- `Cargo.toml`：guest 工程本身的包定义与依赖。
- `case.toml`：runner 读取的测例元数据。
- `build-<target>.toml`：可选。若存在则使用该构建配置；若不存在，runner 会按目标架构生成默认配置。
- `src/main.rs`：guest 入口与测例逻辑。
- `vm/<arch>.toml.in`：启动该 guest 的 VM 模板。模板中至少需要有 `[base]` 与 `[kernel]` 两个段，runner 会在运行时改写其中的 `base.id` 和 `kernel.kernel_path`。

### `case.toml` 字段要求

最小必须字段：

- `id`：测例唯一标识。
- `arch`：该测例支持的架构列表。
- `timeout_secs`：runner 等待该测例结果的超时时间，单位为秒。

可选字段：

- `tags`：标签列表，便于分类、检索和后续统计。
- `description`：测例描述。

示例：

```toml
id = "example.pass_report"
tags = ["example"]
arch = ["aarch64"]
timeout_secs = 5
description = "Minimal example that reports pass."
```

### guest 输出与结束要求

最基本的要求是：

- guest 必须向串口输出一条合法 JSON 结果记录，runner 以此判断测例结果；
- 最小字段为 `case_id` 和 `status`；
- `status` 当前支持 `pass`、`fail`、`skip`、`error`；
- `message` 和 `details` 为**可选字段**。其中 `message` 为简短的描述性文本，`details` 为任意合法 JSON 值，用于提供更多详细信息。

runner 读取的不是“裸 JSON 文件”，而是一段串口输出中的结果片段。也就是说，guest 在结果 JSON 前后仍然可以有普通日志输出，但真正用于 runner 提取的结果需要被固定首尾标志包围。一个最小原始示例如下：

```text
AXTEST_RESULT_BEGIN
{
  "case_id": "example.pass_report",
  "status": "pass",
  "message": "example guest reported pass",
  "details": {
    "example": "pass",
    "value": 1
  }
}
AXTEST_RESULT_END
```

推荐做法：

- 直接使用 `test-suit/axvisor/common/guestlib` 提供的格式化输出方法，例如 `emit_case_pass`、`emit_case_fail`、`emit_case_skip`、`emit_case_error`；这些方法会自动输出带有 `AXTEST_RESULT_BEGIN` / `AXTEST_RESULT_END` 包围的结果记录；
- 在输出结果后立即走统一的结束路径，例如调用 `power_off_or_hang()`。

### suite 要求

suite 使用 TOML 描述，并按架构列出要串行执行的 case。case 路径相对于 `test-suit/axvisor/` 解析。示例：

```toml
name = "axvisor-examples"

[arches.aarch64]
cases = [
  "example/pass-report",
  "example/fail-report",
]
```
