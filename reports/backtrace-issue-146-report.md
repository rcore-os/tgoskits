### 1) 概述（TL;DR）

- 本次进展：补齐 raw backtrace 的结构化输出块（BACKTRACE_BEGIN/BT/BACKTRACE_END），并新增一个 ArceOS QEMU 用例用于验证 raw backtrace 至少能输出两帧并以 `test pass` 结束。
- 产物：ArceOS Rust 用例 `test-suit/arceos/rust/backtrace/`，通过 `cargo xtask arceos test qemu` 运行并用 regex 断言。
- PR：rcore-os/tgoskits#619

### 2) 环境与复现步骤

- 基线：`upstream/dev`
- 验证命令（x86_64）：

```bash
cargo xtask arceos test qemu --target x86_64-unknown-none --package arceos-backtrace
```

### 3) 修复内容（按 PR）

- PR 标题：`axbacktrace: raw backtrace report + ArceOS backtrace test`
- 动机：
  - 需要一个稳定、可机器解析的 raw backtrace 输出格式，用于后续 host-side symbolize（以及 CI 断言）。
  - 需要一个最小的端到端用例，证明 backtrace 输出块确实能在 QEMU 中出现并能被 regex 稳定匹配。
- 实现要点：
  - raw backtrace 输出块：`BACKTRACE_BEGIN kind=... arch=... alloc=... dwarf=...` + 多行 `BT <idx> ip=0x... fp=0x...` + `BACKTRACE_END`
  - ArceOS 测试用例：启动后 `capture()` → 打印 report(kind=test) → 打印 `test pass` → `system_off()`
  - build config：设置 `BACKTRACE=y`，确保目标侧启用 frame pointers；并确保链接脚本参数按目标 rustflags 注入
- 验证证据（关键日志片段）：

```text
Running backtrace tests...
BACKTRACE_BEGIN kind=test arch=x86_64 alloc=true dwarf=false
BT 0 ip=0xffff800000200ca6 fp=0xffff80000025dda0
BT 1 ip=0xffff80000020c399 fp=0xffff80000025ddb0
...
BACKTRACE_END

test pass
```

### 4) 已知限制与 TODO

- 当前用例只验证 raw backtrace block 的存在与基本形态（至少两帧）；不验证 DWARF 符号化与行号。
- TODO：在 host-side symbolize 完整闭环后，把该用例扩展为 “raw → symbolize” 的证据（或新增一个单独用例）。
