# TGOSKits 贡献完整示例

本文档以，演示两种最常见的贡献场景：

- **场景 A（新增）**：以 `examples/tgmath` 为例说明从零创建一个新的组件
- **场景 B（修改）**：以 `examples/tgmath` 为例说明修改组件功能

---

## 1. 获取仓库并创建分支

### 1.1 Fork 仓库

1. 打开 [https://github.com/rcore-os/tgoskits](https://github.com/rcore-os/tgoskits)
2. 点击页面右上角的 **Fork** 按钮
3. 选择你的 GitHub 账户作为 Fork 目标

### 1.2 克隆到本地

```bash
# 将 <YOUR_USERNAME> 替换为你的 GitHub 用户名
git clone https://github.com/<YOUR_USERNAME>/tgoskits.git
cd tgoskits
```

### 1.3 添加上游仓库

```bash
git remote add upstream https://github.com/rcore-os/tgoskits.git
```

验证远程仓库配置：

```bash
git remote -v
# 预期输出：
# origin    https://github.com/<YOUR_USERNAME>/tgoskits.git (fetch)
# origin    https://github.com/<YOUR_USERNAME>/tgoskits.git (push)
# upstream  https://github.com/rcore-os/tgoskits.git (fetch)
# upstream  https://github.com/rcore-os/tgoskits.git (push)
```

### 1.4 同步上游最新代码

```bash
git fetch upstream
git checkout dev
git merge upstream/dev
```

### 1.5 创建功能分支

TGOSKits 使用 `main` / `dev` / 功能分支 三层策略（详见 [docs/repo.md](repo.md)）：

- `main`：稳定发布分支，**禁止直接 push**
- `dev`：集成分支，所有开发通过 PR 合入
- 功能分支：开发者基于 `dev` 创建的个人开发分支

```bash
# 从最新的 dev 创建功能分支
# 分支命名约定：feat/<功能名> 或 fix/<修复名>
git checkout -b feat/add-tgmath-component    # 场景 A
git checkout -b feat/tgmath-add-lcm          # 场景 B
```

---

## 2. 场景 A：新增组件

### 2.1 确定改动位置

TGOSKits 的组件按职责分布在不同目录中，正式添加组件应该放到对应的目录下。但是，作为演示示例，我们将示例添加的组件放在 `examples/` 目录中！

| 你的目标 | 修改位置 |
| --- | --- |
| 通用基础能力（错误、锁、容器等） | `components/` |
| ArceOS 内核模块 | `os/arceos/modules/` |
| ArceOS API / 用户库 | `os/arceos/api/` 或 `os/arceos/ulib/` |
| StarryOS 内核 | `os/StarryOS/kernel/` |
| Axvisor 运行时 | `os/axvisor/src/` |
| 平台适配 | `platform/` 或 `components/axplat_crates/` |

为了同时演示新增和修改组件，`examples/tgmath` 本身已经存在了。对于新增组件的演示，请先删除 `examples/tgmath` 后执行后续步骤！

### 2.2 创建组件目录

根据 [docs/components.md](components) 第 5.2 节定义的标准目录结构，一个完整的组件应包含以下文件：

```text
my_component/
├── Cargo.toml                  # Crate 元数据和依赖配置
├── rust-toolchain.toml         # Rust 工具链配置
├── LICENSE                     # 许可证文件
├── README.md                   # 项目简介
├── .gitignore                  # Git 忽略规则
├── .github/
│   ├── config.json             # CI/CD 组件配置
│   └── workflows/
│       ├── check.yml           # 代码检查工作流
│       ├── test.yml            # 测试工作流
│       ├── deploy.yml          # 文档部署工作流
│       ├── push.yml            # 同步到父仓库
│       └── release.yml         # 发布工作流
├── scripts/
│   ├── check.sh                # 代码检查脚本
│   └── test.sh                 # 测试脚本
├── tests/                      # 集成测试文件
└── src/                        # 组件源码目录
    └── lib.rs                  # 库入口
```

如果组件仅作为 TGOSKits 内部组件（非独立仓库），不需要立即添加 `.github/`、`scripts/` 等 CI 文件。下文所有路径均以 `examples/tgmath/` 为例。

```bash
# 本演示放在 examples/ 下；正式贡献请替换为对应目录（如 components/）
mkdir -p examples/tgmath/src
mkdir -p examples/tgmath/tests
mkdir -p examples/tgmath/scripts
mkdir -p examples/tgmath/.github/workflows
```

### 2.3 编写 `Cargo.toml`

创建 `examples/tgmath/Cargo.toml`：

```toml
[package]
name = "tgmath"
version = "0.1.0"
edition = "2024"
authors = ["TGOSKits Contributor <example@example.com>"]
description = "A tiny math utility crate for TGOSKits demo."
license = "GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0"

[dependencies]
```

### 2.4 编写库代码

创建 `examples/tgmath/src/lib.rs`：

```rust
#![no_std]

/// Add two numbers.
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// Subtract `b` from `a`.
pub fn sub(a: i64, b: i64) -> i64 {
    a - b
}

/// Clamp a value within a range `[lo, hi]`.
pub fn clamp(val: i64, lo: i64, hi: i64) -> i64 {
    if val < lo {
        lo
    } else if val > hi {
        hi
    } else {
        val
    }
}

/// Compute the greatest common divisor.
pub fn gcd(a: u64, b: u64) -> u64 {
    let mut a = a;
    let mut b = b;
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Compute the least common multiple.
pub fn lcm(a: u64, b: u64) -> u64 {
    if a == 0 || b == 0 {
        0
    } else {
        a / gcd(a, b) * b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
        assert_eq!(add(-1, 1), 0);
    }

    #[test]
    fn test_sub() {
        assert_eq!(sub(5, 3), 2);
    }

    #[test]
    fn test_clamp() {
        assert_eq!(clamp(5, 0, 10), 5);
        assert_eq!(clamp(-1, 0, 10), 0);
        assert_eq!(clamp(15, 0, 10), 10);
    }

    #[test]
    fn test_gcd() {
        assert_eq!(gcd(12, 8), 4);
        assert_eq!(gcd(7, 0), 7);
    }

    #[test]
    fn test_lcm() {
        assert_eq!(lcm(4, 6), 12);
        assert_eq!(lcm(0, 5), 0);
        assert_eq!(lcm(7, 0), 0);
        assert_eq!(lcm(3, 7), 21);
    }
}
```

### 2.5 编写集成测试

创建 `examples/tgmath/tests/integration.rs`：

```rust
use tgmath::{add, clamp, gcd, lcm, sub};

#[test]
fn integration_add_sub() {
    assert_eq!(add(100, 200), 300);
    assert_eq!(sub(300, 200), 100);
}

#[test]
fn integration_clamp_boundary() {
    assert_eq!(clamp(0, 0, 100), 0);
    assert_eq!(clamp(100, 0, 100), 100);
}

#[test]
fn integration_gcd_coprime() {
    assert_eq!(gcd(13, 7), 1);
}

#[test]
fn integration_lcm() {
    assert_eq!(lcm(12, 8), 24);
    assert_eq!(lcm(3, 7), 21);
}
```

### 2.6 注册到 Workspace

新增 crate 后，**必须手动将其添加到根 `Cargo.toml` 的 `[workspace] members` 列表中**。TGOSKits 的 workspace members 是显式枚举的，不会通过 glob 模式自动包含。因此，编辑根 `Cargo.toml`，在 `members` 数组末尾添加新行：

```toml
[workspace]
members = [
    # ... 已有成员 ...

    # 新增组件（本演示放在 examples/ 下）
    "examples/tgmath",         # ← 添加这一行
    # 正式组件则使用对应路径，如：
    # "components/tgmath",
]
```

添加后可以验证 workspace 是否识别到新 crate：

```bash
cargo test -p tgmath
```

### 2.7（可选）如果是 subtree 管理的组件

如果新组件有独立仓库，则需要使用 `scripts/repo/repo.py` 工具，将独立仓库的组件显示添加到当前主仓库中。

```bash
python3 scripts/repo/repo.py add \
  --url https://github.com/<org>/tgmath \
  --target examples/tgmath \
  --branch dev \
  --category ArceOS
```

> 本例中 `tgmath` 仅作为演示，不需要注册 subtree。

---

## 3. 场景 B：修改已有 demo

本场景演示修改 [examples/tgmath/](https://github.com/rcore-os/tgoskits/tree/main/examples/tgmath)——为其添加一个 `lcm`（最小公倍数）函数。

### 3.1 添加新函数

编辑 [examples/tgmath/src/lib.rs](https://github.com/rcore-os/tgoskits/blob/main/examples/tgmath/src/lib.rs)，在 `gcd` 函数之后添加 `lcm` 函数：

```rust
/// Compute the least common multiple.
pub fn lcm(a: u64, b: u64) -> u64 {
    if a == 0 || b == 0 {
        0
    } else {
        a / gcd(a, b) * b
    }
}
```

### 3.2 添加单元测试

在 `#[cfg(test)]` 模块中添加测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ... 已有测试 ...

    #[test]
    fn test_lcm() {
        assert_eq!(lcm(4, 6), 12);
        assert_eq!(lcm(0, 5), 0);
        assert_eq!(lcm(7, 0), 0);
        assert_eq!(lcm(3, 7), 21);
    }
}
```

### 3.3 添加集成测试

在 `tests/integration.rs` 中添加测试用例：

```rust
use tgmath::{add, clamp, gcd, lcm, sub};

#[test]
fn integration_lcm() {
    assert_eq!(lcm(12, 8), 24);
    assert_eq!(lcm(3, 7), 21);
}
```

### 3.4 不需要改 workspace members

因为 `examples/tgmath` 已经在 workspace members 中，不需要重复添加。直接修改源码文件即可。

### 3.5 运行测试验证修改

```bash
cargo test -p tgmath
```

预期输出：

```
running 5 tests
test tests::test_add ... ok
test tests::test_sub ... ok
test tests::test_clamp ... ok
test tests::test_gcd ... ok
test tests::test_lcm ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 4 tests
test integration_add_sub ... ok
test integration_clamp_boundary ... ok
test integration_gcd_coprime ... ok
test integration_lcm ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

---

## 4. 本地测试

无论时新增组件还是，修改已有组件，都必须进行完整的本地开发测试。TGOSKits 采用**渐进式验证策略**：从最小消费者开始，逐步扩大验证范围。

### 4.1 第一步：单元测试和 Clippy

首先运行单元测试和静态检查：

```bash
# 运行该 crate 的单元测试
cargo test -p tgmath

# 运行 Clippy 检查（项目约定：不使用 allow 跳过警告，修复根因）
cargo clippy -p tgmath -- -D warnings

# 格式化代码（项目约定：修改代码后必须运行）
cargo fmt
```

预期输出：

```
running 4 tests
test tests::test_add ... ok
test tests::test_sub ... ok
test tests::test_clamp ... ok
test tests::test_gcd ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 4.2 第二步：集成测试

```bash
cargo test -p tgmath --test integration
```

### 4.3 第三步：运行最小系统验证

修改基础组件后，需要确认不影响现有系统。从最轻量的入口开始：

```bash
# ArceOS 最小验证
cargo arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf
```

如果改动涉及特定功能（网络、块设备等），换对应示例：

```bash
# 带网络的验证
cargo arceos qemu --package ax-httpclient --target riscv64gc-unknown-none-elf
```

### 4.4 第四步：运行统一测试

确认改动稳定后，运行完整的 CI 测试矩阵：

```bash
# Host / std crate 测试（等价于 CI 中的 test_std job）
cargo xtask test

# ArceOS 测试（等价于 CI 中的 test_os_target job）
cargo arceos test qemu --target riscv64gc-unknown-none-elf
cargo arceos test qemu --target aarch64-unknown-none-softfloat

# StarryOS 测试
cargo starry test qemu --target riscv64

# Axvisor 测试
cargo axvisor test qemu --target aarch64
```

---

## 5. 提交代码

当完成新增组件或者修改已有组件，并本地测试通过后，就可以继续提交到远程仓库，并进一步向当前主仓库提交 PR 来贡献上游了。

### 5.1 检查改动

```bash
# 查看改动的文件
git status

# 查看具体改动
git diff
```

### 5.2 暂存改动

**场景 A：**

```bash
# 添加所有改动文件
git add examples/tgmath/

# 或逐个添加
git add examples/tgmath/Cargo.toml
git add examples/tgmath/src/lib.rs
git add examples/tgmath/tests/integration.rs
```

**场景 B：**

```bash
# 添加修改的文件
git add examples/tgmath/src/lib.rs
git add examples/tgmath/tests/integration.rs
```

### 5.3 编写提交信息

TGOSKits 遵循 [Conventional Commits](https://www.conventionalcommits.org/) 规范：

```
<type>(<scope>): <subject>

<body>
```

**类型（type）**：

| 类型 | 用途 |
| --- | --- |
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档变更 |
| `refactor` | 代码重构 |
| `test` | 测试相关 |
| `chore` | 构建、工具、CI 等变更 |

**示例：**

**场景 A：**

```bash
git commit -s -m "feat(tgmath): add tgmath utility crate to examples

Add a tiny math utility crate providing add, sub, clamp, gcd and lcm
functions. The crate is no_std compatible and includes unit tests
and integration tests."
```

**场景 B：**

```bash
git commit -s -m "feat(tgmath): add lcm function to examples/tgmath

Add least common multiple (lcm) function to the tgmath demo.
Includes unit and integration tests."
```

> `-s` 参数会自动添加 `Signed-off-by:` 行，表示你同意 Developer Certificate of Origin (DCO)。

### 5.4 推送到 Fork

```bash
# 场景 A
git push origin feat/add-tgmath-component

# 场景 B
git push origin feat/tgmath-add-lcm
```

---

## 6. 提交 PR

> 本节对两个场景通用。

### 6.1 创建 Pull Request

1. 打开你 Fork 的仓库页面：`https://github.com/<YOUR_USERNAME>/tgoskits`
2. GitHub 会自动提示你有一个新推送的分支，点击 **Compare & pull request**
3. 或者手动进入 `https://github.com/rcore-os/tgoskits/compare`

### 6.2 选择目标分支

**重要**：PR 必须指向 `dev` 分支，**禁止直发 `main`**。

```
base: dev  ←  compare: feat/add-tgmath-component
```

### 6.3 填写 PR 标题和描述

PR 标题同样遵循 Conventional Commits 格式：

```text
<type>(<scope>): <subject>
```

其中 `scope` 默认优先写本次改动的主要影响 crate 名；如果改动没有单一主 crate，而是偏向仓库级、文档或 CI，则也可以使用更宽的 scope，比如 `ci`、`repo`、`docs`。

**场景 A：**

```
feat(tgmath): add tgmath utility crate to examples
```

**场景 B：**

```
feat(tgmath): add lcm function to examples/tgmath
```

**场景 C：**

```
chore(ci): split Starry self-hosted board matrix
```

PR 描述模板：

```markdown
## 改动说明

<!-- 场景 A -->
新增 `examples/tgmath` 工具 crate，提供基础数学运算函数。

<!-- 场景 B -->
为 `examples/tgmath` 新增 `lcm` 函数，并补充了单元测试和集成测试。

## 改动范围

- [x] 新增 `examples/tgmath/`（场景 A）
- [x] 修改 `examples/tgmath/src/lib.rs`、`tests/integration.rs`（场景 B）

## 测试

- [x] `cargo test -p tgmath` — 通过
- [x] `cargo clippy -p tgmath -- -D warnings` — 通过
- [x] `cargo fmt` — 通过
- [x] `cargo arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf` — 通过（场景 A）

## 关联 Issue

Closes #<issue_number>（如有）
```

### 6.4 等待 CI 和 Review

提交 PR 后，CI 会自动运行以下检查（对应 `.github/workflows/test.yml`）：

1. **fmt**：`cargo fmt --all -- --check`
2. **test_std**：`cargo xtask test`
3. **test_os_target**：ArceOS / StarryOS / Axvisor 在各架构下的 QEMU 测试

等待 CI 通过后，项目维护者会进行代码审查。根据 Review 意见修改代码后，直接 push 到同一分支即可更新 PR：

```bash
# 修改代码后
git add <changed-files>
git commit -s -m "refactor(tgmath): address review feedback"
git push origin <your-branch-name>
```

### 6.5 合并后的自动同步

PR 合并到 `dev` 后，`push.yml` 工作流会自动检测变更并推送到各组件仓库的 `dev` 分支（如果该组件是 subtree 管理的）。对于非 subtree 组件，合并即完成。

---

## 7. 附录：常见场景速查

### 7.1 不同类型改动的验证路径

| 改动位置 | 第一步验证 | 第二步验证 |
| --- | --- | --- |
| `components/` 下基础 crate | `cargo test -p <crate>` | `cargo arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf` |
| `os/arceos/modules/*` | `cargo arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf` | 需要功能时换 `httpserver`/`shell` |
| `os/StarryOS/kernel/*` | `cargo starry qemu --arch riscv64` | `cargo starry test qemu --target riscv64` |
| `os/axvisor/src/*` | `cargo axvisor qemu --config os/axvisor/.build.toml ...` | `cargo axvisor test qemu --target aarch64` |
| 文档 `docs/*` | 直接提交 PR | — |

### 7.2 常用命令速查

```bash
# 格式化
cargo fmt

# Clippy 检查
cargo clippy -- -D warnings

# 单 crate 测试
cargo test -p <crate-name>

# ArceOS 构建/运行
cargo arceos build --package <pkg> --target <triple>
cargo arceos qemu --package <pkg> --target <triple>

# StarryOS
cargo starry rootfs --arch riscv64
cargo starry qemu --arch riscv64

# Axvisor
cargo axvisor defconfig qemu-aarch64
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)

# 统一测试入口
cargo xtask test

# Subtree 管理
python3 scripts/repo/repo.py list
python3 scripts/repo/repo.py push <component> -b dev
```

### 7.3 分支策略图

```
feature/* ──PR──► dev ──push.yml──► 独立组件仓库 dev 分支
                   │
                定期合并
                   │
                   ▼
           main ◄──PR── 独立组件仓库 main 更新
```

### 7.4 注意事项

1. **永远不要直接 push 到 `main`**——`main` 是稳定基线
2. **PR 指向 `dev` 分支**——所有开发变更先进 `dev`
3. **修改后运行 `cargo fmt`**——项目有格式检查 CI
4. **运行 `cargo clippy`，不使用 `allow` 跳过警告**——修复根因
5. **提交信息加 `-s`**——DCO 签名
6. **优先从仓库根目录运行命令**——`cargo arceos`/`cargo starry`/`cargo axvisor` 统一入口

---

> 两个场景的 demo 代码位于 [examples/tgmath/](https://github.com/rcore-os/tgoskits/tree/main/examples/tgmath)。场景 A 描述的是创建全新组件到 `components/` 的完整流程；场景 B 描述的是直接修改 [examples/tgmath/](https://github.com/rcore-os/tgoskits/tree/main/examples/tgmath) demo 的流程。你可以用 `cargo test -p tgmath` 始终验证两种场景的改动效果。
