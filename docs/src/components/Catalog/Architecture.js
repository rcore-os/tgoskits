import React from 'react';
import ComponentGraph from './ComponentGraph';
import styles from './styles.module.css';

export default function Architecture({entries}) {
  const groupCounts = [...new Set(entries.map(item => item.group))].map(group => ({group, category: entries.find(item => item.group === group).category, count: entries.filter(item => item.group === group).length}));
  const edgeCount = entries.reduce((total, entry) => total + entry.dependencies.length, 0);
  return (
    <section id="component-architecture" className={styles.graphSection} aria-labelledby="component-architecture-title">
      <span id="component-layers" /><span id="component-dependencies" />
      <div className={styles.graphSectionInner}>
        <div className={styles.graphHeading}><div><p className={styles.sectionEyebrow}>COMPONENT MAP</p><h2 id="component-architecture-title">层级与依赖，一图展开。</h2></div><div className={styles.graphMetrics}><span><strong>3</strong> 顶层系统</span><span><strong>{entries.length}</strong> 组件</span><span><strong>{edgeCount}</strong> 依赖关系</span></div></div>
        <p className={styles.graphIntroduction}>从顶部三套系统进入，向下查看系统集成、共享领域和平台分组。图中包含当前目录的全部节点与依赖；点击节点可突出关联路径，使用缩放和滚动查看细节。</p>
        <ComponentGraph entries={entries} />
        <div className={styles.graphNotes}><p><strong>如何读图</strong> 虚线连接系统与所属集成组件；实线箭头从依赖者指向被依赖者。按领域分组表达职责，跨层、同层和向上的连线保留实际声明，不将它们改画为单向分层。</p><p><strong>统计口径</strong> 读取目录内普通依赖和目标条件依赖，合并可选项；不含开发、构建、外部或传递依赖。它是声明关系的全集，不等于某个 target / feature 组合的实际构建图。</p></div>
        <details className={styles.countBreakdown}><summary>软件包数量与收录范围</summary><p>收录共享组件、宏、平台及系统集成软件包；排除测试用例目录、示例、xtask、应用和外部注册表依赖。不等于 workspace 全部成员数。</p><dl>{groupCounts.map(item => <div key={item.group}><dt>{item.category} <code>{item.group}/</code></dt><dd>{item.count}</dd></div>)}</dl></details>
      </div>
    </section>
  );
}
