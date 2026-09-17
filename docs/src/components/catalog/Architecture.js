import React from 'react';
import ComponentGraph from './ComponentGraph';
import styles from './styles.module.css';

export default function Architecture({entries}) {
  const edgeCount = entries.reduce((total, entry) => total + entry.dependencies.length, 0);
  return (
    <section id="component-architecture" className={styles.graphSection} aria-labelledby="component-architecture-title">
      <span id="component-layers" /><span id="component-dependencies" />
      <div className={styles.graphSectionInner}>
        <div className={styles.graphHeading}><div><p className={styles.sectionEyebrow}>COMPONENT MAP</p><h2 id="component-architecture-title">层级与依赖，一图展开。</h2></div><div className={styles.graphMetrics}><span><strong>3</strong> 顶层系统</span><span><strong>{entries.length}</strong> 组件</span><span><strong>{edgeCount}</strong> 依赖关系</span></div></div>
        <p className={styles.graphIntroduction}>ArceOS、StarryOS 与 AxVisor 共享领域组件，并通过系统集成和平台适配连接应用与硬件。下图包含目录内全部组件及其直接依赖。</p>
        <ComponentGraph entries={entries} />
      </div>
    </section>
  );
}
