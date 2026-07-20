import {themes as prismThemes} from 'prism-react-renderer';

const routes = {
  docsOverview: '/docs/introduction/overview',
  quickstart: '/docs/quickstart/overview',
  architecture: '/docs/architecture/overview',
  build: '/docs/build/overview',
  components: '/docs/components',
  arceos: '/docs/development/arceos',
  starryos: '/docs/development/starryos',
  axvisor: '/docs/development/axvisor',
  componentGraph: '/docs/development/components',
  blog: '/blog',
  community: '/community/introduction',
  github: 'https://github.com/rcore-os/tgoskits',
};

/** @type {import('@docusaurus/types').Config} */
const config = {
  title: 'TGOSKits',
  tagline: '面向操作系统与虚拟化开发的统一集成工作区 —— ArceOS · StarryOS · Axvisor',
  favicon: 'images/site/favicon.ico',
  url: 'https://rcore-os.cn',
  baseUrl: '/tgoskits/',
  trailingSlash: false,
  organizationName: 'rcore-os',
  projectName: 'tgoskits',
  deploymentBranch: 'gh-pages',
  onBrokenLinks: 'throw',
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'throw',
    },
    mermaid: true,
  },
  themes: ['@docusaurus/theme-mermaid'],
  plugins: [
    [
      '@docusaurus/plugin-content-docs',
      {
        id: 'community',
        path: 'community',
        routeBasePath: 'community',
        sidebarPath: './sidebars.community.js',
        editUrl: 'https://github.com/rcore-os/tgoskits/tree/main/docs/community',
        showLastUpdateAuthor: true,
        showLastUpdateTime: true,
      },
    ],
  ],
  i18n: {
    defaultLocale: 'zh-Hans',
    locales: ['zh-Hans'],
  },
  presets: [
    [
      'classic',
      {
        docs: {
          path: 'docs',
          routeBasePath: 'docs',
          sidebarPath: './sidebars.docs.js',
          editUrl: 'https://github.com/rcore-os/tgoskits/tree/main/docs',
          showLastUpdateAuthor: true,
          showLastUpdateTime: true,
        },
        blog: {
          path: 'blog',
          routeBasePath: 'blog',
          blogSidebarTitle: 'All posts',
          blogSidebarCount: 'ALL',
          showLastUpdateAuthor: true,
          showLastUpdateTime: true,
          showReadingTime: true,
          feedOptions: {
            type: ['rss', 'atom'],
            xslt: true,
          },
          editUrl: 'https://github.com/rcore-os/tgoskits/tree/main/docs/blog',
          onInlineTags: 'warn',
          onInlineAuthors: 'warn',
          onUntruncatedBlogPosts: 'warn',
        },
        theme: {
          customCss: './src/css/custom.css',
        },
      },
    ],
  ],
  themeConfig: {
    algolia: {
      appId: 'HETXPBD08D',
      apiKey: '12cfa7e19141075aac2d0644cfda70c4',
      // Set this after the first crawler run creates the index.
      indexName: 'tgoskits',
      contextualSearch: true,
      searchPagePath: 'search',
    },
    colorMode: {
      defaultMode: 'light',
      disableSwitch: false,
      respectPrefersColorScheme: true,
    },
    docs: {
      sidebar: {
        hideable: true,
        autoCollapseCategories: true,
      },
    },
    tableOfContents: {
      minHeadingLevel: 2,
      maxHeadingLevel: 4,
    },
    navbar: {
      title: 'TGOSKits',
      logo: {
        alt: 'TGOSKits Logo',
        src: 'images/site/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docs',
          position: 'left',
          label: 'Document',
        },
        {
          to: routes.blog,
          activeBasePath: 'blog',
          label: 'Blog',
          position: 'left',
        },
        {
          to: routes.community,
          activeBasePath: 'community',
          label: 'Community',
          position: 'left',
        },
        {
          href: routes.github,
          position: 'right',
          label: 'GitHub',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: '文档',
          items: [
            {label: '项目概览', to: routes.docsOverview},
            {label: '快速开始', to: routes.quickstart},
            {label: '架构设计', to: routes.architecture},
            {label: '构建与运行', to: routes.build},
          ],
        },
        {
          title: '系统',
          items: [
            {label: 'ArceOS', to: routes.arceos},
            {label: 'StarryOS', to: routes.starryos},
            {label: 'Axvisor', to: routes.axvisor},
            {label: '组件库', to: routes.components},
          ],
        },
        {
          title: '资源',
          items: [
            {label: 'GitHub 仓库', href: routes.github},
            {label: '构建系统', to: routes.build},
            {label: '组件依赖图', to: routes.componentGraph},
            {label: 'Blog', to: routes.blog},
            {label: 'Community', to: routes.community},
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} TGOSKits Contributors. 基于 Docusaurus 构建。`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
    },
  },
};

export default config;
