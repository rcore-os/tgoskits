# TGOSKits 实验进度备忘（Redis + Busybox + 课程实验3）

> 单一续作入口。新对话请先让 AI 读本文件。

## 最后更新

2026-05-22

---

## 术语对照（必读）

来源：[rcore-os/linux-compatible-testsuit#9](https://github.com/rcore-os/linux-compatible-testsuit/issues/9)

| 名称 | 含义 | TA / 备注 |
|------|------|-----------|
| **官方方案一** | 1～2 个 **syscall 导向**的内核改进 | Mr Graveyard |
| **官方方案二** | **Linux 应用导向**的内核/测例改进（如 Redis） | debian: fluove_top；**TA 已确认 Redis 可作为方案二选题**（2026-05-22 沟通） |
| **官方方案三** | **移动机器人全栈**（sg2002 / rk3588） | master-ajax / zhourui747；需先完成方案一+二实践，再联系 TA 转入 |
| **课程实验3** | QEMU 上修 bug/加功能，支持 1～2 个小应用；**busybox 必做** | 与官方方案编号**无关** |

**易混点（已纠正）：** busybox / 实验3 **不是**官方方案三；它是课程必做或官方方案二的延续工作。此前误把 busybox 标成「方案三」——已在本文件修正。

### 本人进度（对照官方方案）

| 官方方案 | 对应工作 | 状态 |
|----------|----------|------|
| 方案一风格 | [#807](https://github.com/rcore-os/tgoskits/pull/807) syscall/VFS rename + ext4 dentry | **已合入** |
| 方案二 | [#808](https://github.com/rcore-os/tgoskits/pull/808) Redis app QEMU 测例 | **OPEN**，等 review；选题 **TA 已认可** |
| 方案三（机器人） | — | **尚未开始**；需联系 TA 确认能否转入 |
| **选题占位** | Redis / 方案二 | **流程待 TA 确认**（飞书文档 vs tgoskits issue，TA 暂不确定） |
| 课程实验3（busybox） | 待开 `fix/starry-busybox-remove-shell` | **未开始**（与方案三独立） |

**下一步优先级：** ① 等/推进 #808（方案二） ② 向 TA 询问能否开始**官方方案三（移动机器人）** + ACT 是否配板 ③ busybox（实验3 必做）按需并行 ④ ACT 板子定型后再排 cgroup/docker/NPU（见下节）。

---

## 课程实验要求（截图摘要）

- **实验3**：在 QEMU 上修 bug / 加功能，支持 **1～2 个小应用**。
- **至少一个 busybox fix 合入 `dev`**；**busybox 为必做**（红字）。
- 小应用路线：Redis（rename/ext4 + app 测例）已基本完成内核侧；Busybox 为下一**课程必做**项（非官方方案三）。

---

## 总览状态表

| 工作线 | 对照 | 分支 / PR | 状态 | 下一步 |
|--------|------|-----------|------|--------|
| syscall/VFS **方案一风格** | 官方方案一 | `fix/starry-rename-cross-parent` → [#807](https://github.com/rcore-os/tgoskits/pull/807) | **MERGED**（2026-05-21） | 收工；本地 `git pull origin dev` |
| Redis app **方案二** | 官方方案二 | `feat/starry-app-redis` → [#808](https://github.com/rcore-os/tgoskits/pull/808) | **OPEN** | 等 review；必要时 rebase `dev` |
| Promin3 大 Redis+TCP | 方案二相关 | `feat/starryos-redis-app` → [#802](https://github.com/rcore-os/tgoskits/pull/802) | **OPEN** | 已留言对齐；勿重复 mount/TCP |
| **Busybox 课程实验3** | 非方案三 | 待开 `fix/starry-busybox-remove-shell` | **未开始** | 从 `dev` 拉分支 |
| **官方方案三（机器人）** | sg2002/rk3588 | — | **未申请** | 联系 TA（见文末备忘） |
| **OS 赛 ACT** | 见下节三档任务 | — | **未选型** | 板子定了再开 cgroup/docker/NPU 等子课题 |

---

## 三条线区分 · OS 赛 ACT · 本周计划

> 来源对话：[方案术语与 busybox 纠正](02c29762)、[Redis 方案二与 TA](b759bfe3)、[ACT 板子与本周计划](0af8ca00)。  
> **OS 赛 ACT 赛题：** 模型在 StarryOS 上适配与实时推理（全国 OS 设计赛 · 功能挑战）。  
> 四条并行线不要混报：向 TA 汇报**官方方案**时勿把 ACT 赛题或 busybox 实验3 当成方案三进展。

### 四条线对照（易混必读）

| 线 | 是什么 | 硬件 / 环境 | 当前状态 | 与另三线的关系 |
|----|--------|-------------|----------|----------------|
| **官方方案二** | Linux **应用导向**内核/测例（Redis 等） | QEMU 为主 | #807 已合、#808 OPEN；**TA 已认 Redis 选题** | 与 ACT **任务三（QEMU）** 技能栈重叠，但**赛道与评奖独立** |
| **OS 赛 ACT** | 操作系统赛 **独立赛题**（三档任务） | sg2002 / rk3588 / QEMU | **未选型、未开板** | 可与方案二/三**并行积累**，汇报与 PR 标题**分开记** |
| **官方方案三** | **移动机器人全栈**（课程官方第三档） | sg2002 或 rk3588 | **未向 TA 申请** | 需方案一+二实践基础；**≠ busybox、≠ ACT 评奖档位** |
| **课程实验3 · busybox** | 课程 QEMU 必做小应用 | QEMU | 分支未开 | **与官方方案三无关**；属实验3 / 方案二延续 |

**记忆口诀：** 方案二 = Redis 应用；ACT = 赛题三档硬件；方案三 = 机器人 TA 线；busybox = 实验3 必做（不是方案三）。

### OS 赛 ACT · 赛题三档（硬件 ↔ 奖项）

| 任务 | 目标板 / 环境 | 奖项档位（赛方口径） | 与 tgoskits 的关联 | 投入粗估 |
|------|---------------|----------------------|-------------------|----------|
| **任务一** | **sg2002**（RISC-V） | **一等奖线** | 仓库有 sg2002 riscv64 相关配置；生态/文档相对少 | 高：板子 + 驱动 + 赛题栈从零多 |
| **任务二** | **rk3588**（如 OrangePi 5 Plus） | **二等奖** | **board 测例常见**（`board-*.toml`、实体板 CI 矩阵）；与官方方案三 rk3588 路径重合度高 | 中：板子现成、社区与仓库样例多 |
| **任务三** | **QEMU**（如 riscv64 virt） | **三等奖** | 与当前 **#807/#808 Redis QEMU** 完全一致；**无实体板成本** | 低：已验证 `app-redis` PASS |

参考：[Starry-OS discussions/24](https://github.com/orgs/Starry-OS/discussions/24)（sg2002 进展）；tgoskits 板测矩阵以 **OrangePi-5-Plus（rk3588）** 最常见。

**ACT vs 官方方案：** ACT 按赛题评奖；官方方案一/二/三按 issue #9 与 TA 路线。可共用内核 PR，但**选题汇报、时间线、对接人分开记**。

### 本人板子选型（推荐结论）

| 若你的目标 | 推荐板/环境 | 理由 |
|------------|-------------|------|
| **尽快收尾方案二 + 低投入参赛** | **QEMU riscv64（ACT 任务三）** | 与 #807/#808 同一套；无借板成本；任务三投入最低 |
| **要实体板 + 仓库资料最多 + 官方方案三** | **rk3588 / Orange Pi 5 Plus（ACT 任务二）** | tgoskits `board-*.toml`、U-Boot 流程成熟；对接 **zhourui747** |
| **冲 ACT 一等奖且能拿到 sg2002** | **sg2002（ACT 任务一）** | riscv64 与现有技能连续；对接 **master-ajax**；内存紧、驱动/全栈投入最大 |

**当前建议（2026-05-22）：** 本周仍以 **QEMU** 收 #808；问 TA 时顺带确认 **ACT/实验是否配实体板**。若只能买/借一块板且非冲一等奖，**优先 rk3588**，勿默认 sg2002。

### 本周计划（2026-05-22 起）

| 序号 | 动作 | 负责 / 对接 | 完成标准 |
|------|------|-------------|----------|
| 1 | 推进 **#808 merge**（官方方案二收尾） | maintainer review；本地必要时 `rebase origin/dev` | #808 合入 `dev` 或维护者明确与 #802 分工 |
| 2 | **问助教：能否开始官方方案三** | 飞书 / 课程渠道；汇报结构用方案一/二/三，**不提 busybox 当方案三** | 得到「可转入 / 暂不可 / 需补材料」之一 |
| 3 | **按选定板子找 TA** | **sg2002 → master-ajax**；**rk3588 → zhourui747**（或助教指定的另一对接人） | 确认板子申领、镜像/串口、方案三子任务清单 |
| 4 | （并行可选）busybox 实验3 | 自有节奏，不替代 ①②③ | `fix/starry-busybox-remove-shell` 开分支即可 |

**板子未敲定前：** cgroup、docker、NPU 等 ACT/方案三**子课题先不写进本周 commit**，避免在无硬件承诺下空转设计。

### 子课题（板子定了再谈）

以下依赖**实体板型号与赛题/TA 下发的具体任务**，本周**不立项**：

| 子课题 | 为何延后 |
|--------|----------|
| cgroup | 依赖内核 + 用户态工具链在**目标板 rootfs** 上可测 |
| docker / 容器 | 通常要 blk/net + 用户态；板级资源与赛题是否要求未知 |
| NPU / 加速卡 | 强绑定 SoC（rk3588 NPU vs sg2002 侧载）；无板不做驱动选型 |

**触发条件：** ACT 任务档位或官方方案三 TA 回复中**明确硬件** → 再在本节下追加「子课题 × 板子」表。

---

## 方案一风格 · #807（已完成）

**PR：** [fix(starry-test-suit): VFS rename and ext4 dentry fixes for Redis AOF](https://github.com/rcore-os/tgoskits/pull/807)  
**分支：** `fix/starry-rename-cross-parent` → `dev`（已合入）  
**对照：** 官方方案一（syscall 导向内核修复）

### 内核改动要点

| 文件 | 作用 |
|------|------|
| `components/axfs-ng-vfs/src/mount.rs` | `Location::rename`：仅当**源项为目录**且为**目标父目录的祖先**时才 `EINVAL`；修复误拦「普通文件 rename 进子目录」（Redis AOF `temp-rewriteaof-*.aof` → `appendonlydir/...`） |
| `components/rsext4/src/file/delete.rs` | hash-tree 目录下 rename **覆盖**时旧 dentry 删不干净；**htree 先 `lookup_directory_entry` 再删叶块**；`find_named_entry_in_parent` / `remove_named_entry_at` 单遍查找（`unlink` / `delete_file` 不再扫两遍） |

### 测例

- **进 CI：** `test-suit/starryos/normal/qemu-smp1/bugfix/bug-rename-replace`（四架构 `qemu-*.toml`）；`components/rsext4` rename 相关单测
- **未进 CI：** `bug-redis-aof-appendonly` 源码保留，已从 bugfix 四架构 toml **移除**（方案 A：避免未就绪用例拖 CI）

### 合入后本地

```bash
git fetch origin
git checkout dev
git pull origin dev
```

---

## 官方方案二 · #808（进行中）

**PR：** [feat(starry-test-suit): add Redis app QEMU smoke and AOF diagnose cases](https://github.com/rcore-os/tgoskits/pull/808)  
**分支：** `feat/starry-app-redis` → `dev`  
**对照：** 官方方案二（Linux 应用导向）  
**合入顺序：** 必须在 **#807 之后**（#807 已合）

### 测例布局

| 目录 | 说明 |
|------|------|
| `test-suit/starryos/normal/qemu-smp1/app-redis/` | 四架构 `qemu-*.toml`：`apk add redis` + `redis-cli ping` / set-get 冒烟 |
| `test-suit/starryos/normal/qemu-smp1/app-redis-deep/` | 仅 **riscv64**：`redis-benchmark` 较重 |
| `test-suit/starryos/stress/app-redis-aof-diagnose/` | riscv64 stress，**手动诊断**，未进 normal/bugfix 自动 CI |

### 已完成动作

- 本地 **`cargo xtask starry test qemu --arch riscv64 -c app-redis` → PASS**（PONG / set-get / `REDIS_APP_TEST_PASSED`）
- 已 push **`f0e3e94f3`**：review 建议的 `shell_init_cmd` 续行格式 + `app-redis-deep` 收窄 `fail_regex`（`(error) ERR` 行）
- 已在 **#802** 留言：rename/ext4 见 #807；#808 只做轻量 app 测例、**不含 TCP**；大测例 + TCP 以 #802 为主

### 待办

- 等 maintainer **review / merge**，或决定 **#808 vs #802** 测例去重（可关 #808 或只留 `stress/app-redis-aof-diagnose`）
- 可选：#807 合入后在 `dev` 上跑  
  `cargo xtask starry test qemu --stress -c app-redis-aof-diagnose --arch riscv64`  
  记录 PASS/FAIL（FAIL 若像 TCP 问题，等 #802，不必再改 `delete.rs`）

### 验证命令

```bash
git fetch origin
git checkout feat/starry-app-redis
git rebase origin/dev
git push --force-with-lease origin feat/starry-app-redis

cargo xtask starry test qemu --arch riscv64 -c app-redis
cargo xtask starry test qemu --arch riscv64 -c app-redis-deep   # 可选
cargo xtask starry test qemu --stress -c app-redis-aof-diagnose --arch riscv64   # 可选
```

---

## #802 协调要点（Promin3 大 PR）

**PR：** [feat(starryos): support Redis app tests](https://github.com/rcore-os/tgoskits/pull/802)  
**分支：** `feat/starryos-redis-app` → `dev`  
**详细笔记：** 曾写在 `.review-notes-pr802.md`（可能在 `git stash` 里）；下文为内联摘要。

### 与本地分支重叠表

| 区域 | PR #802 | #807 `fix/starry-rename-cross-parent` | #808 `feat/starry-app-redis` |
|------|---------|--------------------------------------|------------------------------|
| `components/axfs-ng-vfs/src/mount.rs` | 同类 rename-into-child-dir 修复 | **有**（已合 #807） | 无 |
| `components/rsext4/src/file/delete.rs` | **无** | **有**（htree dentry + 单遍 find） | 无 |
| TCP（`axnet-ng` listen_table、tcp 等） | **有** | 无 | 无 |
| Redis app 测例 | `normal/qemu-smp1/redis/`、`stress/redis/` | 无 | `app-redis/`、`app-redis-deep/`、`stress/app-redis-aof-diagnose/` |
| Rename 回归 | `bug-rename-file-into-child-dir`（C，bugfix toml） | `bug-rename-replace`、rsext4 单测；`bug-redis-aof` 仅源码 | 已删重复 `bug-redis-aof-appendonly/` |

### 建议（勿重复劳动）

1. **不要**在未协调下同时合 #802 与 #807 的 `mount.rs` 切片——会冲突；#807 已合后，#802 **rebase `dev` 并去掉重复 `mount.rs`**。
2. **#807 的 rsext4 / replace 覆盖** #802 **没有**，AOF/rename 正确性仍依赖 #807。
3. **#808** 与 #802 的 `redis/` 测例**功能重叠**——维护者择一或合并目录；你方轻量冒烟在 `app-redis`，大测例 + TCP 以 #802 为主。
4. #802 的 TCP 修复可独立 cherry-pick；与 rename PR 无硬依赖。

### 已对 #802 的沟通要点（可复制变体）

> VFS rename + ext4 hash-tree dentry 已在 **#807** 合入 `dev`。**#808** 仅 `app-redis` / `app-redis-deep` / stress `aof-diagnose`，不含 TCP。本地 riscv64 `app-redis` 已通过。请 rebase 时去掉与 #807 重复的 `mount.rs`；Redis 大测例 + TCP 以你的 #802 为主，避免两套 normal redis 都合。

---

## 课程实验3 · Busybox（必做 · 非官方方案三）

### 目标

- 修一个 **busybox 相关** StarryOS/VFS 问题，**合入 `dev`**，并加/改 `busybox-tests.sh` 回归。
- 与课程要求对齐：**至少一个 busybox fix** + QEMU 可测。
- **注意：** 此项属于**课程实验3 / 方案二延续**，与**官方方案三（移动机器人）**无关。

### Issue / 测例入口

- 上游 issue 参考：`linux-compatible-testsuit#13`（Busybox 兼容性）
- 本仓库：`test-suit/starryos/normal/qemu-smp1/busybox/`
- 执行脚本：`test-suit/starryos/normal/qemu-smp1/busybox/sh/busybox-tests.sh`（注入 guest 为 `/usr/bin/busybox-tests.sh`）

### 首选任务：`busybox_remove_shell`

- **动机：** 与历史 **#751 add-shell** 对称，涉及 **VFS / rename / unlink**（与 #807 技能栈接近，但用户态表现为 busybox `rm`）。
- **分支名（计划）：** `fix/starry-busybox-remove-shell`
- **起步：**

```bash
git fetch origin
git checkout dev
git pull origin dev
git checkout -b fix/starry-busybox-remove-shell
```

### 备选（更快、测例向）

- `fdflush`、resize **usage 文本** 等：主要改 `busybox-tests.sh` 断言，内核改动可能更小。

### 明确勿碰（省时）

| 项 | 原因 |
|----|------|
| crond **#741** | 深、易踩坑 |
| crontab **#750** | 同上 |
| `insmod` | 模块/权限复杂 |
| 网络深栈 | 与 #802 TCP 重叠，非 busybox 主线 |

### 验证命令

```bash
# 全量 busybox 套件（较慢）
cargo xtask starry test qemu --arch riscv64 -c busybox

# 改脚本后语法检查
bash -n test-suit/starryos/normal/qemu-smp1/busybox/sh/busybox-tests.sh
```

### PR 标题示例（合入 `dev` 时）

`fix(starry-test-suit): busybox remove shell regression` 或 `fix(axfs-ng-vfs): ...`（视主要改动 crate 而定）

---

## TA 沟通备忘（2026-05-22）

- **方案二选题：** TA 确认 **Redis 符合官方方案二**（Linux 应用导向）。
- **选题占位：** 是否需在飞书文档或 tgoskits issue 登记 Redis — **TA 表示流程不确定，待后续确认**。
- **待发消息：** 按官方方案一/二/三结构向 TA 汇报进展（见对话产出）；**勿在 TA 汇报中混入 busybox/课程实验3**。

## 官方方案三（移动机器人）· 待申请

- **硬件：** sg2002 或 rk3588 板级全栈。
- **前置（issue #9）：** 需有官方方案一、方案二的实践基础，再联系 TA 转入。
- **当前：** 方案一风格 #807 已合 + 方案二 #808 进行中；**尚未向 TA 申请转入方案三**。
- **动作：** 向 TA（master-ajax / zhourui747）确认能否开始及对接人；**勿把 busybox 实验3 当作方案三进展汇报**。

---

## 常用命令备忘

```bash
# 同步主线
git fetch origin
git checkout dev && git pull origin dev

# Starry QEMU 测例（优先 xtask）
cargo xtask starry rootfs --arch riscv64          # 首次或 rootfs 变更后
cargo xtask starry test qemu --arch riscv64 -c app-redis
cargo xtask starry test qemu --arch riscv64 -c busybox
cargo xtask starry test qemu --stress -c app-redis-aof-diagnose --arch riscv64

# 单 crate clippy（改内核/组件后）
cargo xtask clippy --package rsext4
cargo xtask clippy --package axfs-ng-vfs
cargo fmt --all

# GitHub PR
gh pr view 807 --web
gh pr view 808 --web
gh pr view 802 --web
gh pr view 808 --comments
```

---

## 新对话如何接上

对 AI 说：

> 请先读仓库根目录的 `WORK_PROGRESS.md`，然后继续：**（填一项）**  
> - 等 #808 review / rebase（官方方案二）  
> - 在 #802 跟进协调  
> - 从 `dev` 开 `fix/starry-busybox-remove-shell` 做 **课程实验3 busybox**（非方案三）  
> - 起草/发送向 TA 询问 **官方方案三（机器人）** 的消息  
> - 读「三条线总览」+ transcript `0af8ca00`，定 ACT 板子或本周计划  

并说明当前分支：`git branch --show-current`。

---

## 相关链接速查

| PR | 标题（英文） | URL |
|----|--------------|-----|
| #807 | fix(starry-test-suit): VFS rename and ext4 dentry fixes… | https://github.com/rcore-os/tgoskits/pull/807 |
| #808 | feat(starry-test-suit): add Redis app QEMU smoke… | https://github.com/rcore-os/tgoskits/pull/808 |
| #802 | feat(starryos): support Redis app tests | https://github.com/rcore-os/tgoskits/pull/802 |

| Issue | 说明 |
|-------|------|
| linux-compatible-testsuit#9 | 官方方案一/二/三定义 |
| linux-compatible-testsuit#13 | Busybox 兼容性（课程实验3） |

---

## 本地笔记 / stash

- `.review-notes-pr802.md`、`.review-notes-redis-branches.md` 可能仍在 stash（`git stash list` → `git stash show -p`）。
- 恢复示例：`git stash pop`（注意冲突）。
