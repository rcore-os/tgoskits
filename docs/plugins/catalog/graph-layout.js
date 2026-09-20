// Keep every catalog package visible; domain rows express ownership, not a topological sort.
function layoutCatalog(entries) {
  const width = 2280;
  const gap = 24;
  const margin = 36;
  const groupWidth = (width - margin * 2 - gap * 2) / 3;
  const rows = [
    ['os/arceos', 'os/StarryOS', 'os/axvisor'],
    ['components', 'memory', 'virtualization'],
    ['drivers'],
    ['fs', 'net', 'platforms'],
  ];
  const groups = [];
  const nodes = [];
  let y = 172;
  for (const row of rows) {
    let rowHeight = 0;
    row.forEach((group, column) => {
      const members = entries.filter(entry => entry.group === group);
      if (!members.length) return;
      const columns = row.length === 1 ? 9 : 3;
      const boxWidth = row.length === 1 ? width - margin * 2 : groupWidth;
      const nodeWidth = (boxWidth - 48 - (columns - 1) * 12) / columns;
      const height = 80 + Math.ceil(members.length / columns) * 48;
      const x = margin + column * (groupWidth + gap);
      groups.push({group, category: members[0].category, x, y, width: boxWidth, height, count: members.length});
      members.forEach((entry, index) => nodes.push({
        ...entry, x: x + 24 + (index % columns) * (nodeWidth + 12),
        y: y + 62 + Math.floor(index / columns) * 48, width: nodeWidth, height: 36,
      }));
      rowHeight = Math.max(rowHeight, height);
    });
    y += rowHeight + 72;
  }
  const byRoute = new Map(nodes.map(node => [node.route, node]));
  if (nodes.length !== entries.length) throw new Error('Catalog graph is missing a package group');
  const edges = nodes.flatMap(node => node.dependencies.map(dependency => {
    const target = byRoute.get(dependency.route);
    if (!target) throw new Error(`Missing dependency node: ${dependency.route}`);
    return {source: node.route, target: target.route};
  }));
  const systems = ['ArceOS', 'StarryOS', 'Axvisor'].map((name, column) => ({
    name, group: rows[0][column], x: margin + column * (groupWidth + gap), y: 30,
    width: groupWidth, height: 84,
  }));
  return {width, height: y - 36, groups, nodes, edges, systems};
}

module.exports = {layoutCatalog};
