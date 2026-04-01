# StarryOS Syscall 与 SMP（S0-6 占位说明）

当前探针与 `starry test qemu` 用例默认按 **单核启动路径** 验证；catalog 中部分条目已标注 `smp2`，尚未接成自动化矩阵。

## 后续可落地项

- 在 `cargo xtask starry test qemu` 可传入的 QEMU 配置或模板中增加 **`-smp 2`**（或与现有 `test-suit` TOML 对齐的等价项），为 `futex` / `ppoll` 等条目提供第二档回归。
- 对 **非确定性** 行为（竞态、唤醒顺序）避免固定 `expected/*.line`；改为超时内子串匹配或结构化日志统计。
- 将 SMP 档位与 **`docs/starryos-syscall-compat-matrix.yaml`** 的 `parity` / `notes` 联动，便于审计「单核已测 / 多核未测」。

## 与现有探针的关系

`read` / `write` / `close` 的零长度或 `EBADF` 类 contract 在 SMP 下通常与单核语义一致，可作为 SMP 冒烟的 **稳定子集**；同步原语类需单独设计用例。
