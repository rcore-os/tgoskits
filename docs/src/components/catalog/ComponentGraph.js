import React, {useEffect, useMemo, useRef, useState} from 'react';
import Link from '@docusaurus/Link';
import {useColorMode} from '@docusaurus/theme-common';
import {layoutCatalog} from '@site/plugins/catalog/graph-layout';
import styles from './graph.module.css';

export default function ComponentGraph({entries}) {
  const graph = useMemo(() => layoutCatalog(entries), [entries]);
  const nodes = useMemo(() => new Map(graph.nodes.map(node => [node.route, node])), [graph]);
  const viewport = useRef(null);
  const svg = useRef(null);
  const [viewportWidth, setViewportWidth] = useState(1100);
  const [zoom, setZoom] = useState(null);
  const [selected, setSelected] = useState('');
  const [showEdges, setShowEdges] = useState(true);
  const {colorMode} = useColorMode();
  const scale = zoom ?? Math.min(viewportWidth / graph.width, 2);
  const entry = nodes.get(selected);
  const dependencies = new Set(entry?.dependencies.map(dependency => dependency.route) || []);
  const consumers = graph.nodes.filter(node => node.dependencies.some(dependency => dependency.route === selected));
  const related = new Set([selected, ...dependencies, ...consumers.map(node => node.route)]);
  const palette = colorMode === 'dark'
    ? {bg: '#223932', panel: '#252c34', node: '#303b45', border: '#465563', text: '#e9eff4', muted: '#afbdc9', edge: '#92a8b8', accent: '#6edcc3', used: '#85b6ff', consumer: '#deb0ff'}
    : {bg: '#e9f4f0', panel: '#ffffff', node: '#f5f8fb', border: '#d7e1e9', text: '#233b4b', muted: '#667d8d', edge: '#8ba3b2', accent: '#087e73', used: '#356ac0', consumer: '#9361b7'};
  useEffect(() => {
    const observer = new ResizeObserver(([entry]) => setViewportWidth(entry.contentRect.width));
    observer.observe(viewport.current);
    return () => observer.disconnect();
  }, []);
  function choose(route) {
    setSelected(route);
    const node = nodes.get(route);
    if (node) viewport.current.scrollTo({left: (node.x + node.width / 2) * scale - viewport.current.clientWidth / 2,
      top: (node.y + node.height / 2) * scale - viewport.current.clientHeight / 2, behavior: 'instant'});
  }
  function resize(next) {
    const view = viewport.current;
    const centerX = (view.scrollLeft + view.clientWidth / 2) / scale;
    const centerY = (view.scrollTop + view.clientHeight / 2) / scale;
    setZoom(next);
    requestAnimationFrame(() => view.scrollTo({left: centerX * next - view.clientWidth / 2, top: centerY * next - view.clientHeight / 2, behavior: 'instant'}));
  }
  function download() {
    const copy = svg.current.cloneNode(true);
    copy.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
    copy.setAttribute('width', graph.width);
    copy.setAttribute('height', graph.height);
    copy.querySelectorAll('[data-node]').forEach(node => node.setAttribute('opacity', '1'));
    copy.querySelectorAll('[data-edge]').forEach(edge => edge.setAttribute('opacity', '.3'));
    const url = URL.createObjectURL(new Blob([new XMLSerializer().serializeToString(copy)], {type: 'image/svg+xml'}));
    const link = document.createElement('a'); link.href = url; link.download = 'tgoskits-components.svg'; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return <div className={styles.explorer}>
    <div className={styles.toolbar}>
      <div className={styles.selection}>
        <select aria-label="定位组件" value={selected} onChange={event => choose(event.target.value)}><option value="">全部组件</option>{entries.map(item => <option key={item.route} value={item.route}>{item.name} · {item.category}</option>)}</select>
      <div className={styles.status} role="status">{graph.nodes.length} 个组件 · {graph.edges.length} 条内部直接依赖{entry ? ` · 已选择 ${entry.name}` : ''}</div>
      </div>
      <div className={styles.controls}>
        <button type="button" aria-label="缩小架构图" disabled={scale <= .15} onClick={() => resize(Math.max(.15, scale / 1.25))}>−</button>
        <output aria-label="缩放比例">{Math.round(scale * 100)}%</output>
        <button type="button" aria-label="放大架构图" disabled={scale >= 2} onClick={() => resize(Math.min(2, scale * 1.25))}>＋</button>
        <button type="button" onClick={() => {setZoom(null); viewport.current.scrollTo(0, 0);}}>适应宽度</button>
        <button type="button" onClick={() => {setZoom(Math.min(viewport.current.clientWidth / graph.width, viewport.current.clientHeight / graph.height)); viewport.current.scrollTo(0, 0);}}>查看全图</button>
        <button type="button" onClick={download}>下载 SVG</button>
      </div>
    </div>
    <div className={styles.legend}><label><input type="checkbox" checked={showEdges} onChange={event => setShowEdges(event.target.checked)} />显示全部依赖</label><span>实线 → 直接依赖</span><span>虚线 ⇢ 系统归属</span><span className={styles.outgoing}>蓝色：所选组件的依赖</span><span className={styles.incoming}>紫色：直接使用方</span></div>
    <div className={styles.viewport} ref={viewport} tabIndex={0} role="region" aria-label="完整组件架构图，可滚动和缩放">
      <svg ref={svg} width={graph.width * scale} height={graph.height * scale} viewBox={`0 0 ${graph.width} ${graph.height}`} role="group" aria-label={`${graph.nodes.length} 个组件、${graph.edges.length} 条直接依赖的分层全图`} style={{display: 'block', margin: '0 auto', fontFamily: 'system-ui, sans-serif'}}>
        <title>TGOSKits 组件层级与依赖全图</title>
        <desc>三套系统位于顶部；下方按系统集成、共享领域和平台分组，展示目录全部软件包及其直接依赖。分组不是严格的拓扑分层，跨层与向上连线均来自依赖声明。</desc>
        <rect width={graph.width} height={graph.height} fill={palette.bg} />
        {graph.groups.map(group => <g key={group.group}>
          <rect x={group.x} y={group.y} width={group.width} height={group.height} rx="16" fill={palette.panel} stroke={palette.border} />
          <text x={group.x + 24} y={group.y + 35} fontSize="20" fontWeight="650" fill={palette.text}>{group.category}</text>
          <text x={group.x + group.width - 24} y={group.y + 35} textAnchor="end" fontSize="16" fill={palette.muted}>{group.group}/ · {group.count}</text>
        </g>)}
        {graph.systems.map(system => <g key={system.name}>
          <rect x={system.x} y={system.y} width={system.width} height={system.height} rx="16" fill={palette.panel} stroke={palette.accent} strokeWidth="2" />
          <text x={system.x + system.width / 2} y={system.y + 51} fontSize="30" fontWeight="700" fill={palette.accent} textAnchor="middle">{system.name}</text>
          <path d={`M${system.x + system.width / 2} 118V166m-6-7 6 7 6-7`} stroke={palette.accent} strokeWidth="2" strokeDasharray="6 5" fill="none" />
        </g>)}
        {[...graph.edges].sort((a, b) => Number(a.source === selected || a.target === selected) - Number(b.source === selected || b.target === selected)).map(edge => {
          const source = nodes.get(edge.source), target = nodes.get(edge.target);
          const outgoing = edge.source === selected, incoming = edge.target === selected;
          const active = outgoing || incoming;
          const downward = target.y > source.y;
          const x1 = source.x + source.width / 2, y1 = downward ? source.y + source.height : source.y;
          const x2 = target.x + target.width / 2, y2 = downward ? target.y : target.y + target.height;
          const direction = downward ? 1 : -1;
          const middle = (y1 + y2) / 2;
          return <g key={`${edge.source}:${edge.target}`} data-edge opacity={active ? 1 : showEdges ? (selected ? .035 : .28) : 0} pointerEvents="none">
            <path d={`M${x1} ${y1}C${x1} ${middle} ${x2} ${middle} ${x2} ${y2}`} fill="none" stroke={outgoing ? palette.used : incoming ? palette.consumer : palette.edge} strokeWidth={active ? 3 : 1.2} />
            <path d={`m${x2 - 4} ${y2 - direction * 6} 4 ${direction * 6} 4 ${-direction * 6}`} fill="none" stroke={outgoing ? palette.used : incoming ? palette.consumer : palette.edge} strokeWidth={active ? 2.5 : 1.2} />
          </g>;
        })}
        {graph.nodes.map(node => <g key={node.route} data-node={node.route} role="button" tabIndex={0} aria-label={`查看 ${node.name} 的依赖`} aria-pressed={selected === node.route}
          onClick={() => choose(node.route)} onKeyDown={event => {if (event.key === 'Enter' || event.key === ' ') {event.preventDefault(); choose(node.route);}}}
          opacity={!selected || related.has(node.route) ? 1 : .38} style={{cursor: 'pointer'}}>
          <title>{node.name} · {node.location}</title>
          <rect x={node.x} y={node.y} width={node.width} height={node.height} rx="6" fill={palette.node} stroke={selected === node.route ? palette.accent : dependencies.has(node.route) ? palette.used : consumers.some(item => item.route === node.route) ? palette.consumer : palette.border} strokeWidth={related.has(node.route) ? 2.5 : 1} />
          <text x={node.x + node.width / 2} y={node.y + 24} textAnchor="middle" fontSize="15" fill={palette.text}
            textLength={node.name.length > 25 ? node.width - 14 : undefined} lengthAdjust="spacingAndGlyphs">{node.name}</text>
        </g>)}
      </svg>
    </div>
    {entry && <div className={styles.inspector}>
      <><div className={styles.inspectorHeading}><div><h3>{entry.name}</h3><p>{entry.description}</p></div><div><Link to={entry.route}>组件详情</Link><button type="button" onClick={() => setSelected('')}>清除选择</button></div></div>
        <div className={styles.relations}><div><h4>直接依赖 · {entry.dependencies.length}</h4>{entry.dependencies.length ? entry.dependencies.map(item => <button type="button" key={item.route} onClick={() => choose(item.route)}>{item.name}</button>) : <p>无目录内直接依赖。</p>}</div><div><h4>直接使用方 · {consumers.length}</h4>{consumers.length ? consumers.map(item => <button type="button" key={item.route} onClick={() => choose(item.route)}>{item.name}</button>) : <p>无目录内直接使用方。</p>}</div></div></>
    </div>}
  </div>;
}
