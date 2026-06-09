# 贡献完整示例

TGOSKits 的顶层 `examples/` 目录用于放置可运行的场景示例，而不是通用
workspace 组件模板。系统级或组件级示例仍应优先放在对应子系统目录中，例如
`apps/arceos/`、`components/*/examples/` 或 `test-suit/`。

## StarryOS 板端示例

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

TGOSKits 使用 `main` / `dev` / 功能分支 三层策略（详见 [仓库管理](/docs/contributing/repo)）：

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
| 平台适配 | `platforms/` |

为了同时演示新增和修改组件，`examples/tgmath` 本身已经存在了。对于新增组件的演示，请先删除 `examples/tgmath` 后执行后续步骤！

### 2.2 创建组件目录

根据 [组件开发指南](/docs/development/components) 第 5.2 节定义的标准目录结构，一个完整的组件应包含以下文件：

```text
apps/starry/<case>/
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

### 2.7 Subtree 组件（可选）

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
cargo arceos qemu --package arceos-std-helloworld --target riscv64gc-unknown-none-elf
```

如果改动涉及特定功能（网络、块设备等），换对应示例：

```bash
# 带网络的验证
cargo arceos qemu --package arceos-std-httpclient --target riscv64gc-unknown-none-elf
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
init.sh
build-<target>.toml
board-<board>.toml
```

运行方式：

```bash
cargo starry app board -t <case>
```

`init.sh` 会被 `cargo starry app board` 读取并作为 Starry shell 的启动命令发送到
板端；`board-<board>.toml` 继续提供 board type、shell prefix、匹配规则和超时；
`build-<target>.toml` 提供 StarryOS 内核构建配置。

第一个 StarryOS 场景示例是：

```bash
cargo starry app board -t orangepi-5-plus-uvc
```

该示例假设板端 rootfs 已经预装 `/usr/bin/uvc-fps` 以及 `libuvc`、`libusb` 等运行时
依赖。示例目录中附带的 Rust std 项目用于构建这个用户态程序，但不会被 root
workspace 自动构建。

## 新增组件或示例

- 新增通用可复用组件时，放到合适的 `components/`、`drivers/`、`platforms/` 或
  `os/*/modules/` 子目录，并同步 workspace、文档和验证白名单。
- 新增 ArceOS 应用示例时，优先使用 `apps/arceos/`。
- 新增 StarryOS 板端场景时，使用 `apps/starry/<case>/`，并确保 case 可以通过
  `cargo starry app board -t <case>` 被发现。
- 新增 CI 回归用例时，使用 `test-suit/`，不要把 CI-only 行为混入顶层 examples。
