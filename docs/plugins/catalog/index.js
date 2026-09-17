const fs = require('node:fs');
const path = require('node:path');
const {parse} = require('smol-toml');

const groups = {
  components: ['基础组件', '/docs/architecture/overview'],
  drivers: ['设备驱动', '/docs/architecture/driver/overview'],
  memory: ['内存管理', '/docs/architecture/memory/overview'],
  virtualization: ['虚拟化', '/docs/quickstart/axvisor'],
  fs: ['文件系统', '/docs/architecture/fs/overview'],
  net: ['网络协议', '/docs/architecture/net/overview'],
  platforms: ['平台适配', '/docs/architecture/platform/overview'],
  'os/arceos': ['ArceOS 组件', '/docs/architecture/arceos'],
  'os/StarryOS': ['StarryOS 组件', '/docs/architecture/starryos'],
  'os/axvisor': ['Axvisor 组件', '/docs/architecture/axvisor'],
};
const ignored = new Set(['target', '.git', 'node_modules', 'test_crates', 'tests', 'examples', 'xtask']);
const sourceBase = 'https://github.com/rcore-os/tgoskits/tree/main/';

function directories(directory) {
  return fs.readdirSync(directory, {withFileTypes: true})
    .filter((entry) => entry.isDirectory() && !ignored.has(entry.name))
    .map((entry) => path.join(directory, entry.name));
}

function manifests(directory) {
  const own = path.join(directory, 'Cargo.toml');
  return [...(fs.existsSync(own) ? [own] : []), ...directories(directory).flatMap(manifests)];
}

function readme(directory) {
  const name = ['README_CN.md', 'README.zh-CN.md', 'README.md']
    .find((file) => fs.existsSync(path.join(directory, file)));
  if (!name) return {};
  const content = fs.readFileSync(path.join(directory, name), 'utf8');
  const paragraphs = content.replace(/```[\s\S]*?```/g, '').split(/\n\s*\n/);
  const summary = paragraphs.find((paragraph) => {
    const text = paragraph.trim();
    return text.length > 25 && !/^(#|[<>!|\[\-*]|\d+\.)/.test(text);
  });
  return {
    readme: name,
    summary: summary?.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
      .replace(/[*`]/g, '').replace(/\s+/g, ' ').trim(),
  };
}

function collectCatalog(root) {
  const workspaceConfig = parse(fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8')).workspace;
  const workspace = workspaceConfig.package;
  const packageManifests = new Map();
  const relative = (directory) => path.relative(root, directory).split(path.sep).join('/');
  const components = Object.entries(groups).flatMap(([group, [category, guide]]) =>
    manifests(path.join(root, group)).flatMap((manifest) => {
      const data = parse(fs.readFileSync(manifest, 'utf8'));
      if (!data.package) return [];
      const pkg = data.package;
      const directory = path.dirname(manifest);
      const location = relative(directory);
      packageManifests.set(location, data);
      const info = readme(directory);
      const inherited = (field) => pkg[field]?.workspace ? workspace[field] : pkg[field];
      return [{
        name: pkg.name, category, group, location,
        route: `/components/${location}`,
        description: inherited('description') || info.summary || `${pkg.name}：${category}软件包。`,
        version: inherited('version'), license: inherited('license'),
        tags: inherited('keywords') || [],
        features: Object.keys(data.features || {}),
        source: sourceBase + location,
        readme: info.readme ? `${sourceBase}${location}/${info.readme}` : null,
        guide,
      }];
    }),
  );
  const packagesByDirectory = new Map(components.map(entry => [path.resolve(root, entry.location), entry]));
  for (const entry of components) {
    const data = packageManifests.get(entry.location);
    const dependencyTables = [data.dependencies, ...Object.values(data.target || {}).map(target => target.dependencies)];
    const dependencies = new Map();
    for (const table of dependencyTables) {
      for (const [alias, declaration] of Object.entries(table || {})) {
        const dependency = declaration.workspace ? workspaceConfig.dependencies?.[alias] : declaration;
        if (!dependency?.path) continue;
        const base = declaration.workspace ? root : path.join(root, entry.location);
        const resolved = packagesByDirectory.get(path.resolve(base, dependency.path));
        if (resolved) dependencies.set(resolved.route, {name: resolved.name, route: resolved.route});
      }
    }
    entry.dependencies = [...dependencies.values()].sort((a, b) => a.name.localeCompare(b.name, 'en'));
  }
  const appsRoot = path.join(root, 'apps');
  const appDirectories = directories(appsRoot).flatMap((directory) => {
    const name = path.basename(directory);
    if (name === 'common') return [];
    return ['arceos', 'starry'].includes(name) ? directories(directory) : [directory];
  });
  const apps = appDirectories.map((directory) => {
    const location = relative(directory);
    const system = location.split('/')[1];
    const category = {arceos: 'ArceOS', starry: 'StarryOS'}[system] || '开发工具';
    const info = readme(directory);
    const manifest = path.join(directory, 'Cargo.toml');
    const pkg = fs.existsSync(manifest) ? parse(fs.readFileSync(manifest, 'utf8')).package : null;
    const configurations = fs.readdirSync(directory)
      .filter((name) => /^(qemu|board)-.*\.toml$/.test(name)).sort();
    return {
      name: path.basename(directory), category, group: system, location,
      route: `/${location}`,
      description: info.summary || pkg?.description || `${category} 中的 ${path.basename(directory)} 应用与运行入口。`,
      tags: [...new Set(configurations.map((name) => name.split('-')[1].replace('.toml', '')))],
      configurations,
      source: sourceBase + location,
      readme: info.readme ? `${sourceBase}${location}/${info.readme}` : null,
      guide: system === 'arceos' ? '/docs/build/arceos/overview' :
        system === 'starry' ? '/docs/build/starry/app' : '/docs/ci/testing/applications',
    };
  });
  for (const entries of [components, apps]) {
    entries.sort((a, b) => a.name.localeCompare(b.name, 'en'));
    const routes = new Set();
    for (const entry of entries) {
      if (routes.has(entry.route)) throw new Error(`Duplicate catalog route: ${entry.route}`);
      routes.add(entry.route);
    }
  }
  return {components, apps};
}

module.exports = function catalogPlugin(context) {
  const root = path.resolve(context.siteDir, '..');
  return {
    name: 'tgoskits-catalog',
    getPathsToWatch() {
      return [...Object.keys(groups), 'apps']
        .map((directory) => path.join(root, directory, '**/*.{toml,md}'))
        .concat(path.join(root, 'Cargo.toml'));
    },
    loadContent() { return collectCatalog(root); },
    async contentLoaded({content, actions}) {
      const {createData, addRoute, setGlobalData} = actions;
      setGlobalData(content);
      for (const [kind, entries] of Object.entries(content)) {
        for (const entry of entries) {
          const data = await createData(`${kind}-${entry.route.replaceAll('/', '-')}.json`,
            JSON.stringify({kind, entry}));
          addRoute({
            path: `${context.siteConfig.baseUrl}${entry.route.slice(1)}`, exact: true,
            component: '@site/src/templates/CatalogDetail.js', modules: {catalog: data},
          });
        }
      }
    },
  };
};
