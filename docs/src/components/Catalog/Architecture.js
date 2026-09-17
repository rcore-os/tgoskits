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
        <p className={styles.graphIntroduction}>ArceOS、StarryOS 与 AxVisor 共享领域组件，并通过系统集成和平台适配连接应用与硬件。下图包含目录内全部组件及其直接依赖。</p>
        <ComponentGraph entries={entries} />
        <details className={styles.countBreakdown}><summary>软件包数量与收录范围</summary><p>收录共享组件、宏、平台及系统集成软件包；排除测试用例目录、示例、xtask、应用和外部注册表依赖。不等于 workspace 全部成员数。</p><p>依赖统计包含普通依赖、目标条件及可选项，不含开发、构建、外部或传递依赖；实际启用关系取决于 target 和 feature。</p><dl>{groupCounts.map(item => <div key={item.group}><dt>{item.category} <code>{item.group}/</code></dt><dd>{item.count}</dd></div>)}</dl></details>
      </div>
    </section>
  );
}
