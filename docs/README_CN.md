<h2 align="center">TGOSKits Docs</h2>

<p align="center">TGOSKits 的 Docusaurus 文档站点源码。</p>

<div align="center">

[![GitHub stars](https://img.shields.io/github/stars/rcore-os/tgoskits?logo=github)](https://github.com/rcore-os/tgoskits/stargazers)
[![GitHub forks](https://img.shields.io/github/forks/rcore-os/tgoskits?logo=github)](https://github.com/rcore-os/tgoskits/network)
[![license](https://img.shields.io/github/license/rcore-os/tgoskits)](https://github.com/rcore-os/tgoskits/blob/main/LICENSE.Apache2)

</div>

[English](README.md) | 中文

# 简介

本目录保存 TGOSKits 文档站点的源码，文档站点基于 [Docusaurus](https://docusaurus.io/) 构建。

站点内容主要包括：

- 项目介绍
- 快速开始
- 设计与实现文档
- 使用手册
- 社区页面
- Blog 内容

## 开发

### 环境要求

文档站点本质上是一个 Node.js 应用，当前项目使用 `yarn` 作为包管理器。

推荐环境：

1. Node.js 18 或更高版本
2. 执行 `corepack enable`，或自行安装全局 `yarn`
3. 本地克隆 `https://github.com/rcore-os/tgoskits`

### 安装依赖

在 `docs/` 目录下执行：

```bash
corepack enable
yarn install --frozen-lockfile
```

### 本地预览

启动开发服务器：

```bash
yarn start
```

构建静态站点：

```bash
yarn build
```

本地预览构建结果：

```bash
yarn serve
```

## 目录结构

常用目录和文件如下：

- `docs/docs/`：主文档内容
- `docs/blog/`：Blog 内容
- `docs/community/`：社区文档
- `docs/src/pages/`：由 Docusaurus 自动注册的首页、OSs、Components、应用案例页面
- `docs/src/components/catalog/`：目录页面共用的架构图、依赖图、图标和样式
- `docs/src/templates/CatalogDetail.js`：插件注册的组件与应用详情模板
- `docs/plugins/catalog/`：构建时扫描目录、生成数据与注册路由
- `docs/src/css/`：站点主题和公共尺寸变量
- `docs/src/components/layout/page.module.css`：页面共用的布局与交互样式
- `docs/static/`：静态资源
- `docs/docusaurus.config.js`：站点配置
- `docs/sidebars.docs.js`：主文档侧边栏
- `docs/sidebars.community.js`：社区文档侧边栏

自定义页面共用 `src/components/layout/page.module.css` 的 `container`、`hero`、`heroInner`、`split`、`section`、`copy`、`description`、`featureList`、`actions`、`visual` 和按钮样式。页面样式通过 CSS Modules 的 `composes` 复用这些规则；`src/css/custom.css` 统一维护 80% 桌面内容宽度、整屏 Hero、高度上限、章节间距、标题字号和主题色。图示颜色、交替排版和依赖图工具栏等领域样式保留在各自模块，普通文档的阅读列继续由 Docusaurus 主题管理。

`useVisualHeight()` 返回页面根节点的 ref，仅观察该页面的 `data-visual-pair` 与 `data-visual-copy`，更新共享的 `--visual-copy-height`。插图通过 `visual` 和自身宽高比等比缩放；卸载页面时断开观察器。不要再为页面追加另一套 Hero 高度、文字拉伸或局部图文高度覆盖。

首页由 `src/pages/index.js` 和 `src/pages/index.css` 维护，保留终端演示、组件关系、三套系统、四层架构、仓库同步、硬件平台、验证与文档导航。`ArchitectureIllustration()` 维护四层架构图，`ComponentWorkspaceDiagram()` 维护仓库同步图。Components 首屏的层级框架图位于 `static/images/showcase/component-hierarchy.svg`，以 ArceOS、StarryOS、Axvisor 为顶层，说明系统集成、共享组件与平台适配的四层逻辑视图，不代替逐包依赖图。

OSs 菜单进入 `/oss`，由 `src/pages/oss.js` 按 ArceOS、AxVisor、Starry 的顺序组织三套系统的架构图和说明。`static/images/oss/` 保存各系统的完整 SVG，页面以内联 SVG 继承明暗主题，也提供原图入口；`oss.module.css` 维护响应式图文布局，桌面内容区沿用站点的 80% 宽度。

## 组件与应用目录

导航中的 Components 和应用案例分别进入 `/components` 与 `/apps`。Components 支持搜索与分类筛选。Components 依次展示整屏架构介绍 Hero、COMPONENT MAP 层级依赖全图和全部组件卡片；总体介绍集中在 Hero，不再设置重复介绍区或背景动画按钮。背景动效播放一次，并遵循减少动态效果设置；`src/components/catalog/Architecture.js` 组织说明，`ComponentGraph.js` 展示全部目录节点和依赖，支持缩放、定位、高亮直接依赖与使用方以及 SVG 下载。`plugins/catalog/graph-layout.js` 按系统集成、共享领域和平台归属排列节点，不把领域层次当成依赖的拓扑层次，最下方以等宽等高的卡片网格展示全部目录条目；卡片提供摘要和功能数量，点击后查看完整详情。手机端 Hero 按内容自然增高，框架图可点击打开原始 SVG。应用案例由 `src/pages/apps.js` 的 `products` 选择 PostgreSQL、Nginx、llama.cpp、Redis 和 FFmpeg，以图文说明应用场景、功能范围和运行准备，配置与源码入口仍来自插件扫描数据。页面只展示精选案例，保留 `/apps` 和已有详情路由；新增案例需核对应用 README 与运行配置。动效遵循系统的减少动态效果设置。

`plugins/catalog/index.js` 的 `collectCatalog()` 在站点启动或构建时生成目录：组件取自 `components/`、`drivers/`、`memory/`、`virtualization/`、`fs/`、`net/`、`platforms/` 和三套系统目录下的 Cargo 软件包，排除测试、示例与 xtask 目录；应用取自 `apps/arceos/`、`apps/starry/` 的直接子目录及 `apps/` 顶层工具目录，排除共享脚本目录 `common`。组件以 Cargo.toml 为元数据来源，应用优先使用 README 的正文摘要。运行配置标签仅表示仓库存在对应配置，不代表当前持续集成结果。

新增条目无需维护页面清单。更新对应软件包的 Cargo.toml、README 或应用目录后，执行 `yarn build` 即可更新列表和详情页；缺少 README 时仍保留源码入口。`src/pages/components.js` 与 `src/pages/apps.js` 通过 `usePluginData()` 读取目录插件用 `setGlobalData()` 提供的数据，列表路由由 Docusaurus 自动注册。插件只用 `addRoute()` 为每个条目注册详情路由，统一使用 `src/templates/CatalogDetail.js`。共享图标和标题位于 `src/components/catalog/Emblem.js`、`titles.js`，详情模板不依赖列表页。运行网站需要完整仓库，以便插件读取这些目录。

目录依赖从普通依赖及目标条件依赖的本地路径解析，支持 workspace 继承与依赖重命名；可选项一并收录，同一目标软件包去重。它不包含开发、构建、外部或传递依赖，也不等价于指定 feature 与 target 后的实际构建图。依赖发现规则可通过 `yarn test` 在 `docs/` 目录验证。

## 部署

当前文档站点发布到 GitHub Pages：

- 站点地址：`https://rcore-os.github.io/tgoskits/`

仓库已经配置 GitHub Actions 自动部署文档。Pages 工作流会在 `docs/` 目录中构建 Docusaurus 站点，并将生成的 `docs/build` 发布到 GitHub Pages。

## 如何贡献

欢迎为文档贡献内容，包括：

- 修改或新增 Markdown 文档
- 调整导航结构
- 修正文档中的命令和链接
- 优化页面样式和展示效果

常见流程如下：

1. 修改 `docs/docs/` 下的文档内容
2. 在 `docs/` 目录执行 `yarn start`
3. 本地预览
4. 提交 PR

## 许可协议

本文档站点属于 `rcore-os/tgoskits` 仓库的一部分。许可信息请参阅仓库根目录下的相关许可证文件。
