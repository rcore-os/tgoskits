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

运行 **`scripts/run_linux_guest_oracle.sh`**（仓库已提供）时：

- **`STARRY_LINUX_GUEST_IMAGE`**（必需）：riscv64 **Linux 内核镜像**路径（`Image` / `vmlinuz` 等）。  
- **`QEMU_SYSTEM_RISCV64`**：默认 `qemu-system-riscv64`。  
- **`STARRY_LINUX_GUEST_TIMEOUT`**：秒，默认 `90`。  
- **`STARRY_LINUX_GUEST_APPEND`**：追加到内核 cmdline（可选）。
- **`STARRY_LINUX_GUEST_CC`**（可选）：用于编译 initramfs 里 **控制台 stub** 的 riscv64 交叉 `gcc`；未设置时尝试 `CROSS_COMPILE`+`gcc` 或常见 `riscv64-*-gcc`。

脚本将 **控制台 stub** 编为 **`/init`**（打开 `/dev/console` 后 `exec` 探针），探针 ELF 为 **`/probe`**，打成 gzip cpio 后用 `qemu-system-riscv64 -machine virt -kernel … -initrd …` 启动；**不再使用**单独的 `STARRY_LINUX_GUEST_INITRD` 输入文件。

### 相关命令

- 单探针串口输出：`STARRY_LINUX_GUEST_IMAGE=… test-suit/starryos/scripts/run-diff-probes.sh oracle-guest <basename>`  
- 与 `guest-alpine323` 期望比对：`VERIFY_ORACLE_TRACK=guest-alpine323 STARRY_LINUX_GUEST_IMAGE=… …/run-diff-probes.sh verify-oracle-all`（无内核且 `VERIFY_STRICT=0` 时跳过 guest 校验）。  
- 批量重写 golden：`STARRY_LINUX_GUEST_IMAGE=… ./scripts/refresh_guest_oracle_expected.sh`  
- 物化批次（guest 轨）：`python3 scripts/materialize_syscall_batch.py --batch … --oracle-track guest-alpine323 --guest-kernel /path/to/Image`
- 校验矩阵行在仓库内已提交 guest 金线：`python3 scripts/check_compat_matrix.py --require-guest-golden`

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
| 2026-04-02 | 阶段 B：在自备 `linux-image/Image`（gitignore，见仓库根 `linux-image/`）上执行 `scripts/refresh_guest_oracle_expected.sh`，已重写 `expected/guest-alpine323/*.line` 共 **209** 份；`VERIFY_ORACLE_TRACK=guest-alpine323 VERIFY_STRICT=1 … verify-oracle-all` 通过。若干探针（如 `recvmsg_badfd`）轨 B 与轨 A errno 可能不同，以 guest 金线为准。 |
| 2026-04-02 | 阶段 C：`scripts/starryos-probes-ci.sh` 对兼容矩阵执行 `python3 scripts/check_compat_matrix.py --require-guest-golden`，默认 CI 在无 qemu-system 的情况下校验 **partial/aligned** 行均已提交对应 `expected/guest-alpine323/` 金线；真机全量 oracle 比对仍见 `starryos-linux-guest-oracle` workflow。 |
