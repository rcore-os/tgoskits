---
sidebar_position: 2
sidebar_label: "文档发布"
---

# 文档发布

`.github/workflows/docs.yml` 将 Docusaurus 构建和 GitHub Pages 部署分成两个 job。构建产物通过 Pages artifact 交接，部署 job 不重新编译站点；主 CI 的 Rust 或系统测试不是这条发布链路的前置依赖。

## 1. 触发与调度

文档工作流的路径过滤独立于 `ci.yml`。主 CI 排除 Markdown，并不意味着文档变更不会触发其他自动化。

### 1.1 事件范围

自动触发要求 push 到 `main`，且修改 `docs/**` 或 `.github/workflows/docs.yml`。`dev` 的文档 push 和普通 PR 不在这个工作流的自动触发条件中。

`workflow_dispatch` 支持显式运行，job 没有额外的 ref 或 repository owner 条件。手动运行不是单纯的构建检查：构建成功后会继续尝试部署，实际能否发布取决于 Pages 配置和权限。

### 1.2 并发策略

工作流使用固定的 `docs-pages` concurrency group，并设置 `cancel-in-progress=true`。新运行可以取消同组旧运行，使文档发布优先处理更新的内容。

这与主 CI 对 `main`、`dev` 保留各次提交验证的队列策略不同。文档工作流不承诺为每个历史提交生成一次完整部署。

## 2. 站点构建

`build` job 使用 GitHub-hosted runner，依次准备 Node.js、依赖和站点产物。所有包管理及构建命令在 `docs` 目录执行。

### 2.1 依赖与编译

`actions/setup-node` 固定 Node.js 24，并按 `docs/yarn.lock` 配置 Yarn 缓存。随后启用 Corepack，执行 `yarn install --frozen-lockfile` 和 `yarn build`。

`docs/package.json` 将 `build` 映射到 `docusaurus build`。构建失败时不上传可部署产物，下游 `deploy` 因依赖失败而不执行；Yarn 缓存命中也不能代替锁文件安装和站点编译。

### 2.2 内容与路由

`docs/docusaurus.config.js` 配置主文档目录、community 文档插件、博客、Mermaid 和站点基础路径。`onBrokenLinks` 与 `onBrokenMarkdownLinks` 均为 `throw`，MDX 编译或内部链接错误会使构建失败。

主文档侧栏由 `docs/sidebars.docs.js` 声明顶级分类，再按目录自动生成条目。文档的 `slug` 可以独立于源码目录，所以移动 Markdown 后还需区分文件链接、文档 ID 和公开 URL；当前自动化概览保留 `/docs/build/ci` 作为兼容入口。

`baseUrl=/tgoskits/` 和 `trailingSlash=false` 影响最终路径及静态文件布局。构建输出位于 `docs/build`，不是源码目录 `docs/docs`，也不是 Rust 的 `target`。

## 3. Pages 部署

站点发布使用 Pages 专用 artifact 和 OIDC 权限，而不是直接在构建命令中推送一个网页分支。

### 3.1 产物交接

构建成功后，`actions/configure-pages` 准备 Pages 配置，`actions/upload-pages-artifact` 上传 `docs/build`。`deploy` job 通过 `needs: build` 等待这一过程完成，再调用 `actions/deploy-pages`。

```mermaid
flowchart LR
    source[文档与站点配置] --> build[Node.js 和 Yarn 构建]
    build --> artifact[Pages artifact]
    artifact --> deploy[deploy job]
    deploy --> pages[GitHub Pages]
```

两个 job 分别占用 runner，但存在顺序依赖，不会因为单次工作流的这两个阶段同时启动而占用两台机器。artifact 上传成功只证明产物已交接，部署成功才证明 Pages 接受了该发布。

### 3.2 权限与结果

工作流声明 `contents: read`、`pages: write` 和 `id-token: write`。`deploy` 使用 `github-pages` environment，并将部署步骤返回的 `page_url` 写入 environment URL。

environment 的审批和部署限制属于仓库设置，YAML 中的名称不证明当前设置了哪些保护。构建可以成功而部署因权限或配置失败，这两类结果必须分别记录。

本地 `yarn build` 验证站点内容、配置和内部链接，不上传 artifact、不调用 Pages，也不证明线上部署已完成。它同样不能验证所有外部链接或浏览器中的最终交互行为。
