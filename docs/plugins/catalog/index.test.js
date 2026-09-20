const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {test} = require('node:test');
const catalogPlugin = require('./index');

test('catalog discovers layered packages and resolves declared local dependencies without test-only or external edges', (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tgoskits-catalog-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  for (const folder of ['components', 'drivers', 'memory', 'virtualization', 'fs', 'net', 'platforms', 'os/arceos', 'os/StarryOS', 'os/axvisor', 'apps']) {
    fs.mkdirSync(path.join(root, folder), {recursive: true});
  }
  function write(file, content) {
    fs.mkdirSync(path.dirname(path.join(root, file)), {recursive: true});
    fs.writeFileSync(path.join(root, file), content);
  }
  function pkg(folder, name, content = '') {
    write(`${folder}/Cargo.toml`, `[package]\nname = "${name}"\nversion = "1.0.0"\n${content}`);
  }
  write('Cargo.toml', `[workspace.package]
version = "1.0.0"
[workspace.dependencies]
renamed = { package = "storage-core", path = "memory/storage" }
`);
  pkg('memory/storage', 'storage-core');
  pkg('fs/volume', 'volume-core');
  pkg('components/dev-helper', 'dev-helper');
  pkg('components/test_crates/hidden', 'hidden-test');
  pkg('os/arceos/xtask', 'hidden-tool');
  pkg('os/StarryOS/kernel', 'system-core', `[dependencies]
renamed = { workspace = true, optional = true }
external = "1.0"
[dev-dependencies]
helper = { path = "../../../components/dev-helper" }
[build-dependencies]
helper = { path = "../../../components/dev-helper" }
[target.'cfg(target_arch = "aarch64")'.dependencies]
volume = { package = "volume-core", path = "../../../fs/volume" }
storage-again = { package = "storage-core", path = "../../../memory/storage" }
`);
  const {components} = catalogPlugin({siteDir: path.join(root, 'docs')}).loadContent();
  const entry = components.find(item => item.name === 'system-core');
  assert.ok(entry, 'system-layer packages must be discoverable');
  assert.deepEqual(entry.dependencies.map(item => item.name), ['storage-core', 'volume-core']);
  for (const dependency of entry.dependencies) {
    assert.ok(components.some(item => item.route === dependency.route && item.name === dependency.name), 'dependency links must resolve to generated detail routes');
  }
  assert.ok(components.some(item => item.name === 'dev-helper'), 'a test-only dependency may still be a catalog package');
  assert.ok(!components.some(item => item.name.startsWith('hidden-')), 'test and task-tool directories must be excluded');
});
