---
name: reassign-pr-reviewers
description: 根据讨论、所有权矩阵、开放拉取请求范围或现有审查请求，为 `rcore-os/tgoskits` 分配或重新平衡 GitHub 拉取请求审查人。需要保留机器人请求或处理审查人权限限制时也使用本技能。
---

# 重新分配拉取请求审查人

## 目标

按照仓库的审查人分派事实来源更新开放拉取请求的审查请求。始终把 `.github/MAINTAINERS.md` 作为严格允许列表：每个小节通过 `R:` 列出一个或多个 GitHub 审查人账号，通过 `F:` 给出路径提示，通过 `K:` 给出关键词或方向提示。审查人分配只修改 GitHub 元数据，不修改代码、不执行构建或测试，也不提交代码审查结论。

## 事实来源

1. 确定仓库和当前用户：

   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   ```

2. 分配前读取目标分支或当前工作树中的 `.github/MAINTAINERS.md`。
3. 把每个维护者小节解析为一条所有权规则：
   - `R:` 是可向 GitHub 请求的审查人账号。不要使用只出现在 `M:` 的账号，除非同一账号也在 `R:` 中。
   - `F:` 是路径或通配模式提示，与拉取请求变更文件比较。
   - `K:` 是逗号分隔的关键词或方向提示，与标题、描述、变更路径、软件包名、功能名、配置名及差异中的明显文件名比较。
4. 人类审查人允许集合只能来自 `.github/MAINTAINERS.md` 的 `R:`。其他来源提到但不在 `R:` 中的账号应报告为已忽略，不得请求。
5. 范围不明确时优先使用 `K:` 的明确关键词证据；变更文件清楚落入某维护者范围时也可使用 `F:`。
6. 多个维护者小节同时匹配时，去掉拉取请求作者后请求所有命中的 `R:`，除非事实来源或用户明确要求只选一人。
7. 非草稿拉取请求没有明确 `K:` 或 `F:` 匹配时，在确认 `ZR233` 位于 `R:` 后，使用 `ZR233` 作为默认审查人，并明确说明这是默认分配，不是所有权证据。
8. 默认跳过草稿，不增删其审查人；只有用户明确要求包含草稿时才处理。

只有用户明确要求时，才可使用 GitHub 讨论或具体拉取请求分配表等外部来源把拉取请求映射到所有权区域。外部来源不能扩展 `.github/MAINTAINERS.md` 的 `R:` 允许列表。讨论应直接读取：

```bash
gh api graphql \
  -f query='query($owner:String!,$repo:String!,$number:Int!){ repository(owner:$owner,name:$repo){ discussion(number:$number){ title body url author{login} comments(first:100){nodes{author{login} body createdAt}} } } }' \
  -F owner=<owner> -F repo=<repo> -F number=<discussion>
```

使用外部来源时：

- 来源中逐条列出的拉取请求优先采用明确分配；
- 未列出的开放拉取请求仍按 `.github/MAINTAINERS.md`，先匹配 `K:`，再匹配 `F:`；
- 外部来源点名但未列入 `R:` 的账号一律忽略。

## 收集当前状态

列出全部开放拉取请求及当前审查请求：

```bash
gh pr list --repo <owner>/<repo> --state open --limit 200 \
  --json number,title,body,author,isDraft,reviewRequests,files,updatedAt,url
```

写入时使用请求审查人的精确接口读取状态：

```bash
gh api repos/<owner>/<repo>/pulls/<pr>/requested_reviewers
```

除非用户明确要求移除或重新平衡，默认必须保留现有审查请求。现有人类审查人可能由管理员手工分配，即使不在 `.github/MAINTAINERS.md` 也不得在默认只新增流程中移除。现有机器人请求同样必须保留，除非用户明确要求修改机器人请求。

草稿默认完全跳过，在写入前演练中以 `draft` 原因记录。

## 写入前演练

写入 GitHub 前输出一张演练表，至少包含：

- 拉取请求编号和作者；
- 当前审查人；
- 所有权目标审查人；
- 保留的现有审查人和机器人；
- 待移除和待新增审查人；
- 命中的维护者小节、关键词或路径提示；
- 没有命中时采用的默认审查人；
- 跳过原因。

计算顺序是：先从 `.github/MAINTAINERS.md` 得出所有权目标，再与现有审查请求取并集。默认只新增，因此待移除集合必须为空。只有用户明确要求移除或重新平衡时才计算删除；即使如此，除非用户明确要求移除机器人，否则仍需保留机器人。

每个目标审查人都要报告分派证据：

- `K:`：列出具体命中词，以及它出现在标题、描述、路径、软件包名、配置名还是差异标识符中；
- `F:`：列出匹配的路径模式及实际变更文件；
- 非草稿且无证据：使用 `ZR233`，并说明没有 `K:` 或 `F:` 所有权证据；
- 草稿：明确跳过，不计算增删操作。

讨论中同时存在旧的具体分配表和更宽的所有权矩阵时，说明每组采用的规则：表中拉取请求使用明确分配，后来未列出的拉取请求按 `.github/MAINTAINERS.md` 推导。

## 应用变更

不要使用 `gh pr edit --add-reviewer` 或 `--remove-reviewer`；该命令可能因查询已弃用的经典项目字段而失败。

使用拉取请求的请求审查人接口：

```bash
# 新增审查人。
printf '%s\n' '{"reviewers":["<login>"]}' |
  gh api -X POST repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -

# 删除审查人。
printf '%s\n' '{"reviewers":["<login>"]}' |
  gh api -X DELETE repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -
```

默认只新增时不得调用删除接口。用户明确要求移除或重新平衡时，每个拉取请求先执行允许的删除，再执行新增，同时继续保留未获明确删除授权的机器人。各拉取请求之间可以继续处理，但每次失败都要记录拉取请求编号、目标账号和 GitHub 错误。

## 权限处理

目标用户不是协作者或权限不足时，请求可能失败。失败后检查权限：

```bash
gh api repos/<owner>/<repo>/collaborators/<login>/permission --jq '.permission'
```

GitHub 拒绝某位审查人时：

- 保留该拉取请求上已成功分配的审查人；
- 继续添加 GitHub 接受的其他目标审查人；
- 报告未能添加的账号，以及观察到的权限或接口错误；
- 除非事实来源明确支持，不得用无关审查人替换失败目标。

## 最终核验

写入后重新读取全部开放拉取请求的审查请求：

```bash
gh pr list --repo <owner>/<repo> --state open --limit 200 \
  --json number,author,reviewRequests |
  jq -r 'sort_by(.number)[] | "#\(.number)\t\(.author.login)\t\(.reviewRequests|map(.login // .name // .slug // .__typename)|join(","))"'
```

把最终状态与目标映射逐项比较。最终报告包括：

- 纳入考虑的开放拉取请求数量；
- 成功改变的拉取请求；
- 原本已经符合目标的拉取请求；
- 有意跳过的拉取请求，特别是草稿、机器人作者或只保留现有审查请求的情况；
- 因目标审查人无法请求而只完成部分分配的拉取请求；
- 受阻账号，以及精确权限或接口错误。

如果只改变 GitHub 审查人元数据，明确说明没有执行构建或静态检查。
