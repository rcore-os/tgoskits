# StarryOS vs Linux：已知分歧登记（compat 矩阵）

当 **StarryOS guest** 与 **Linux oracle**（轨 A：`qemu-riscv64`；轨 B：**`qemu-system-riscv64` + Alpine 3.23.3 / Linux 6.18 LTS**，见 **`docs/starryos-linux-guest-oracle-pin.md`**）对同一 contract 的 **`CASE`** 行**长期不一致**，且经评审**不打算改内核以对齐 Linux**（或短期内无法修复）时，在 **`docs/starryos-syscall-compat-matrix.yaml`** 登记为 **`parity: divergent`**，并填写 **`tracking_issue`** 指向跟踪 issue / 设计讨论。

**user oracle 与 guest 内核 oracle 不一致**：若仅轨 A 与轨 B 的期望行不同，优先以 **轨 B（Alpine 锚点）** 作为「真实 Linux 内核」参照；轨 A 差异在 **`notes`** 中说明即可，**不必**登记为 StarryOS `divergent`，除非 StarryOS guest 与轨 B 仍不一致。

## 登记步骤

1. 用 **`run-diff-probes.sh verify-oracle <probe>`** 与 **`run-starry-probe-qemu-smp2.sh <probe>`**（或矩阵日志）固定 **want / got**。
2. 在 **`docs/starryos-syscall-compat-matrix.yaml`** 对应 **`syscall:`** 行（或新增行）设置：
   - **`parity: divergent`**
   - **`tracking_issue: https://github.com/<org>/<repo>/issues/<n>`**（或等效可打开链接）
   - **`notes:`**：简述差异（errno、返回值、触发条件）。
3. 若仍保留 Linux oracle 文件作对照：保留 **`contract_probe`** 与 **`expected/*.line`** 或 **`.cases`**，并在 **`notes`** 说明 guest 期望策略（例如计划中的 **guest 专用期望文件**）。
4. 若不再用固定 oracle 行覆盖该场景：清空 **`contract_probe`**（或删除对应探针矩阵行），但仍须保留 **`tracking_issue`** 与 **`notes`**。

## CI 校验

**`scripts/check_compat_matrix.py`** 要求：凡 **`parity: divergent`** 的行必须带 **`tracking_issue`**，且值为 **`http://`** 或 **`https://`** 开头的 URL（占位链接不接受，以免静默失效）。

## 当前已登记分歧

| syscall | 说明 | tracking_issue |
|---------|------|----------------|
| （无） | 尚无 **`divergent`** 行 | — |

（有登记时在此表同步一行，便于非 YAML 读者检索。）

## 相关文档

- **`docs/starryos-syscall-compat-matrix.yaml`** — 权威字段与 **`notes`**
- **`docs/starryos-syscall-behavior-evidence.md`** — 分发表 syscall 与矩阵/catalog 探针、parity 对照（机器生成）
- **`docs/starryos-syscall-testing-method.md`** — 「StarryOS 与 Linux 不一致时的处理」
- **`docs/starryos-probes-matrix-failure-playbook.md`** — 矩阵失败 triage
