<h2 align="center">TGOSKits Docs</h2>

<p align="center">The Docusaurus-based documentation site for TGOSKits.</p>

<div align="center">

[![GitHub stars](https://img.shields.io/github/stars/rcore-os/tgoskits?logo=github)](https://github.com/rcore-os/tgoskits/stargazers)
[![GitHub forks](https://img.shields.io/github/forks/rcore-os/tgoskits?logo=github)](https://github.com/rcore-os/tgoskits/network)
[![license](https://img.shields.io/github/license/rcore-os/tgoskits)](https://github.com/rcore-os/tgoskits/blob/main/LICENSE.Apache2)

</div>

English | [中文](README_CN.md)

# Introduction

This directory contains the source for the TGOSKits documentation website, built with [Docusaurus](https://docusaurus.io/).

The site covers:

- project introduction
- quick start guides
- design and implementation documents
- manuals
- community pages
- blog content

## Development

### Environment

The documentation site is a Node.js application. The current project uses `yarn` as the package manager.

Recommended environment:

1. Node.js 18 or newer
2. `corepack enable` or a globally installed `yarn`
3. A local clone of `https://github.com/rcore-os/tgoskits`

### Install Dependencies

Run the following commands in the `docs/` directory:

```bash
corepack enable
yarn install --frozen-lockfile
```

### Local Preview

Start the development server:

```bash
yarn start
```

Build the static site:

```bash
yarn build
```

Serve the generated site locally:

```bash
yarn serve
```

## Source Layout

Important directories and files:

- `docs/docs/`: main documentation content
- `docs/blog/`: blog content
- `docs/community/`: community docs
- `docs/src/`: custom React pages and theme code
- `docs/static/`: static assets
- `docs/docusaurus.config.js`: site configuration
- `docs/sidebars.docs.js`: main docs sidebar
- `docs/sidebars.community.js`: community sidebar

## Deployment

The documentation site is published to GitHub Pages:

- Site URL: `https://rcore-os.github.io/tgoskits/`

The repository is configured to deploy the site through GitHub Actions. The Pages workflow builds the Docusaurus site from the `docs/` directory and publishes the generated `docs/build` output.

## Contributing

Contributions are welcome. You can update Markdown documents, add pages, improve navigation, or refine visual presentation.

If you are editing docs content, the most common workflow is:

1. edit files under `docs/docs/`
2. run `yarn start` in `docs/`
3. preview locally
4. submit a PR

## License

The documentation content is part of the `rcore-os/tgoskits` repository. See the repository license files for details.
