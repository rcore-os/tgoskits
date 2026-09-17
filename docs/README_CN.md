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
- `docs/src/`：自定义页面和主题代码
- `docs/static/`：静态资源
- `docs/docusaurus.config.js`：站点配置
- `docs/sidebars.docs.js`：主文档侧边栏
- `docs/sidebars.community.js`：社区文档侧边栏

## 组件与应用目录

导航中的 Components 和 APPS 分别进入 `/components` 与 `/apps`，两页均支持名称、简介和标签搜索，以及分类筛选。Components 使用按内容高度排列的瀑布流，点击组件进入站内详情。APPs 由 `src/components/Catalog/Apps.js` 展示图文交错的应用介绍、能力示意动画及全部应用目录；点击“配置与详情”或应用名称可原地展开运行配置、README、源码和独立详情入口。动效遵循系统的减少动态效果设置，示意图不代表实际运行截图。

`plugins/catalog/index.js` 的 `collectCatalog()` 在站点启动或构建时生成目录：组件取自 `components/`、`drivers/`、`memory/`、`virtualization/` 下的 Cargo 软件包，排除测试与示例目录；应用取自 `apps/arceos/`、`apps/starry/` 的直接子目录及 `apps/` 顶层工具目录，排除共享脚本目录 `common`。组件以 Cargo.toml 为元数据来源，应用优先使用 README 的正文摘要。运行配置标签仅表示仓库存在对应配置，不代表当前持续集成结果。

新增条目无需维护页面清单。更新对应软件包的 Cargo.toml、README 或应用目录后，执行 `yarn build` 即可更新列表和详情页；缺少 README 时仍保留源码入口。`src/components/Catalog/` 统一维护卡片、筛选、详情及响应式样式。运行网站需要完整仓库，以便插件读取这些目录。

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
