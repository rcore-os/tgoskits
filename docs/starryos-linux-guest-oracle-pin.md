# Linux guest oracle 固定锚点（双轨制 · 轨 B）

轨 B 在 **`qemu-system-riscv64`** 内运行 **真实 Linux 内核** 上的静态 riscv64 探针，用于与 **StarryOS system guest** 叙事对齐，并仲裁轨 A（`qemu-riscv64` linux-user）下可能出现的 **伪 ENOSYS** 等偏差。

本仓库**不选择**抽象的「Linux 7.x」主线作为锚点；**固定发行版与内核世代**如下。

## 锚点

| 项 | 固定值 |
|----|--------|
| 发行版 | **Alpine Linux** |
| 发行版版本 | **3.23.3** |
| 对应内核世代 | **Linux 6.18 LTS**（与 Alpine 3.23.x 线一致；以 3.23.3 软件源/镜像中实际 **`linux-*` 包版本号** 为最终真相源） |

实施时请 **钉死** 下列之一，并在变更时更新本文与 `docs/starryos-syscall-compat-matrix.yaml` 的 `linux_profile.guest_linux_anchor`：

- 在 Alpine 3.23.3 环境执行 **`uname -r`** / 查阅 **`/apk/*/main/riscv64/linux-lts-*`**（或实际使用的内核包名）得到的 **完整内核 release 字符串**；或  
- 自行构建的 **同一 `.config` + 同一 tag** 的 `Image`，并记录 **Git 标签与 `sha256sum`**。

## 环境变量（建议）

运行 `scripts/run_linux_guest_oracle.sh`（若已接入）或等价包装时：

- **`STARRY_LINUX_GUEST_IMAGE`**：guest 用的 **riscv64 Linux 内核镜像**路径（如 `Image` 或 `vmlinuz`）。  
- **`STARRY_LINUX_GUEST_INITRD`**（可选）：initramfs；若用「单探针即 `/init`」的 cpio 方案，可由脚本每次生成。  
- **`QEMU_SYSTEM_RISCV64`**：默认 `qemu-system-riscv64`。

## expected 分轨命名

与轨 A（`expected/user/` 或仓库根下历史 `expected/*.line`）区分时，轨 B 的金色输出建议放在：

`test-suit/starryos/probes/expected/guest-alpine323/`

目录名 **`guest-alpine323`** 表示「Alpine 3.23.x 线 guest oracle」，**不等价**于仅写 `6.18`：升级小版本时以 **Alpine 补丁级** 与 **包内内核 release** 为准。

## 与轨 A 的关系

- **轨 A**：`qemu-riscv64`，快速、默认 CI 友好。  
- **轨 B**：本文件锚定的 **Alpine 3.23.3 / 6.18 LTS** 内核行为；争议 syscall 以轨 B 刷新 `guest-alpine323/*.line` 后，在矩阵 `notes` 或 catalog 中注明 golden 来源。

## 变更日志

| 日期 | 说明 |
|------|------|
| （维护者填写） | 初始钉死 Alpine 3.23.3 + 6.18 LTS 锚点 |
