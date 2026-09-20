const assert = require('node:assert/strict');
const {test} = require('node:test');
const {layoutCatalog} = require('./graph-layout');

test('full graph preserves isolated nodes, cyclic and cross-domain dependencies without overlapping nodes', () => {
  const make = (name, group, dependencies = []) => ({name, group, category: group, route: `/components/${name}`, dependencies: dependencies.map(name => ({name, route: `/components/${name}`}))});
  const entries = [make('runtime', 'os/arceos', ['helper']), make('helper', 'components', ['runtime']),
    make('isolated', 'platforms'), ...Array.from({length: 11}, (_, index) => make(`device-${index}`, 'drivers', ['helper']))];
  const graph = layoutCatalog(entries);
  assert.deepEqual(graph.nodes.map(node => node.route).sort(), entries.map(entry => entry.route).sort());
  assert.deepEqual(graph.edges.map(edge => `${edge.source}>${edge.target}`).sort(), entries.flatMap(entry => entry.dependencies.map(dependency => `${entry.route}>${dependency.route}`)).sort());
  for (const node of graph.nodes) {
    assert.ok(node.x >= 0 && node.y >= 0 && node.x + node.width <= graph.width && node.y + node.height <= graph.height);
    for (const other of graph.nodes) {
      if (node === other) continue;
      assert.ok(node.x + node.width <= other.x || other.x + other.width <= node.x || node.y + node.height <= other.y || other.y + other.height <= node.y, `${node.name} overlaps ${other.name}`);
    }
  }
  assert.throws(() => layoutCatalog([make('unknown', 'new-domain')]), /missing a package group/);
  assert.throws(() => layoutCatalog([make('broken', 'components', ['absent'])]), /Missing dependency node/);
});
