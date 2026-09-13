---
name: crates-io-owner
description: 审计或更新本 TGOSKits 工作区中新软件包的 crates.io 所有者。用户要求为分支新增软件包添加或核验 `github:rcore-os:crates-io`、询问哪些新软件包仍缺少团队所有者，或明确要求使用 `cargo owner` 而不是修改 `Cargo.toml` 时使用。
---

# 管理 crates.io 所有者

本技能直接使用 `cargo owner` 管理分支新增软件包的 crates.io 所有者。不要为此把所有者写入 `Cargo.toml`。

## 工作流程

1. 相对比较基准找出分支新增的 `Cargo.toml`，通常运行：

   ```bash
   git diff --name-status origin/main...HEAD
   ```

2. 只保留与发布有关的真实软件包清单：
   - 优先选择工作区成员；
   - 除非用户明确包含，否则跳过独立示例、测试夹具和辅助软件包；
   - 跳过设置了 `publish = false` 的软件包。
3. 从 `cargo metadata` 或清单文件取得软件包名称。
4. 每次为一个软件包添加所有者：

   ```bash
   cargo owner --add github:rcore-os:crates-io <crate>
   ```

5. 准确处理命令结果：
   - `already an owner` 表示成功但无需修改，应报告所有者原本已经存在；
   - 其他注册表错误应完整报告，并按严重程度决定停止还是继续处理独立软件包。
6. 不要仅为了记录 crates.io 所有权而修改 `Cargo.toml`。

## 推荐命令

列出新增清单：

```bash
python3 - <<'PY'
import subprocess
out = subprocess.check_output(
    ['git', 'diff', '--name-status', 'origin/main...HEAD'],
    text=True,
)
for line in out.splitlines():
    status, path = line.split('\t', 1)
    if status == 'A' and path.endswith('Cargo.toml'):
        print(path)
PY
```

解析候选工作区软件包：

```bash
cargo metadata --no-deps --format-version 1
```

添加所有者：

```bash
cargo owner --add github:rcore-os:crates-io <crate>
```

## 结果报告

有助于说明结果时，按三类汇总：

- 已成功添加所有者；
- `github:rcore-os:crates-io` 原本已经是所有者；
- 已跳过或受阻，并附具体原因。

没有产生文件修改时也要明确说明。

## 约束

- 使用用户要求的 crates.io 操作，不得改成清单元数据修改。
- 除非用户要求更广泛审计，否则范围只包含分支新增软件包。
- 比较基准不明确时默认使用 `origin/main...HEAD`，但分支上下文明确指向其他基准时应采用实际基准。
- 未发布软件包尚不存在于 crates.io，导致 `cargo owner --add` 失败时，如实报告，不要构造本地替代做法。
