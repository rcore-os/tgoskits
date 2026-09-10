# 离线评审约定

本文件标记当前仓库是隔离的 `review-single-pr offline-benchmark` 环境。

- 唯一评审目标是本仓库中 `bench-base` 与 `HEAD` 之间已经提交的变更。
- 当前环境没有真实拉取请求身份或外部上下文。不得联网、操作 GitHub、访问仓库外路径、写入文件、构建或测试。
- 当前 `AGENTS.md`、完整 `.agents/skills/`、为 Claude Code 物化的完整 `.claude/skills/` 和 `.agent-review-context/review.schema.json` 已同时提交到合成变更两侧，只是评审指令，不属于待审差异。
- 最终回答只能包含输出结构要求的 JSON 对象。
