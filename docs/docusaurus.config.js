import {themes as prismThemes} from 'prism-react-renderer';

/** @type {import('@docusaurus/types').Config} */
const config = {
  title: 'TGOSKits',
  tagline: '面向操作系统与虚拟化开发的统一集成工作区 —— ArceOS · StarryOS · Axvisor',
  favicon: 'images/site/favicon.ico',
  url: 'https://rcore-os.github.io',
  baseUrl: '/tgoskits/',
  trailingSlash: false,
  organizationName: 'rcore-os',
  projectName: 'tgoskits',
  deploymentBranch: 'gh-pages',
  onBrokenLinks: 'warn',
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'warn',
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
          to: '/blog',
          activeBasePath: 'blog',
          label: 'Blog',
          position: 'left',
        },
        {
          to: '/community/introduction',
          activeBasePath: 'community',
          label: 'Community',
          position: 'left',
        },
        {
          href: 'https://github.com/rcore-os/tgoskits',
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
            {label: '项目概览', to: '/docs/introduction/overview'},
            {label: '快速开始', to: '/docs/quickstart/overview'},
            {label: '架构设计', to: '/docs/design/architecture/arch'},
            {label: '使用手册', to: '/docs/manual/deploy/qemu'},
          ],
        },
        {
          title: '系统',
          items: [
            {label: 'ArceOS', to: '/docs/design/systems/arceos-guide'},
            {label: 'StarryOS', to: '/docs/design/systems/starryos-guide'},
            {label: 'Axvisor', to: '/docs/design/systems/axvisor-guide'},
            {label: '组件库', to: '/docs/crates'},
          ],
        },
        {
          title: '资源',
          items: [
            {label: 'GitHub 仓库', href: 'https://github.com/rcore-os/tgoskits'},
            {label: '构建系统', to: '/docs/design/build/flow'},
            {label: '组件依赖图', to: '/docs/design/reference/tgoskits-dependency'},
            {label: 'Blog', to: '/blog'},
            {label: 'Community', to: '/community/introduction'},
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
