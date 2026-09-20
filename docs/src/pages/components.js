import useVisualHeight from '../hooks/useVisualHeight';
import layout from '../components/layout/page.module.css';
import React, {useState} from 'react';
import Layout from '@theme/Layout';
import {usePluginData} from '@docusaurus/useGlobalData';
import Link from '@docusaurus/Link';
import useBaseUrl from '@docusaurus/useBaseUrl';
import ComponentArchitecture from '@site/static/images/showcase/component-hierarchy.svg';
import Architecture from '../components/catalog/Architecture';
import Emblem from '../components/catalog/Emblem';
import {titles} from '../components/catalog/titles';
import styles from '../components/catalog/styles.module.css';

const description = '操作系统与虚拟化平台的基础组件、设备驱动和内存管理模块。';

export default function ComponentsPage() {
  const pageRef = useVisualHeight();
  const {components: entries} = usePluginData('tgoskits-catalog');
  const illustration = useBaseUrl('/images/showcase/component-hierarchy.svg');
  const [query, setQuery] = useState('');
  const [category, setCategory] = useState('全部');
  const categories = ['全部', ...new Set(entries.map((entry) => entry.category))];
  const search = query.trim().toLocaleLowerCase();
  const visible = entries.filter((entry) =>
    (category === '全部' || entry.category === category) &&
    [entry.name, entry.description, entry.location, ...entry.tags].join(' ').toLocaleLowerCase().includes(search));
  const reset = () => { setQuery(''); setCategory('全部'); };
  return (
    <Layout wrapperClassName="site-showcase" title={titles.components} description={description}>
      <div className={styles.catalog} ref={pageRef}>
      <header id="architecture-overview" className={styles.hero}>
        <div className={styles.heroBackdrop} aria-hidden="true">
          <div className={styles.glowPrimary} /><div className={styles.glowSecondary} />
          <svg className={styles.backdropLines} viewBox="0 0 1600 1000" preserveAspectRatio="xMidYMid slice" fill="none">
            <g className={styles.flowLines}>
              <path d="M-100 720C180 830 270 170 690 215S1200 480 1700 130" />
              <path d="M-100 754C180 864 270 204 690 249S1200 514 1700 164" />
              <path d="M-100 788C180 898 270 238 690 283S1200 548 1700 198" />
            </g>
            <g className={styles.orbitLines}><circle cx="1300" cy="470" r="400" /><circle cx="1300" cy="470" r="445" /><circle cx="1300" cy="470" r="490" /></g>
            <path className={styles.signalLine} d="M-100 754C180 864 270 204 690 249S1200 514 1700 164" />
          </svg>
        </div>
        <div className={`container ${styles.heroInner}`} data-visual-pair>
          <div className={styles.heroCopy} data-visual-copy>
          <p className={styles.heroLabel}>TGOSKits <span>组件与系统基础</span></p>
          <h1 className={layout.heroTitle}>{titles.components}</h1>
          <p className={styles.intro}>按需组合，跨系统复用。</p>
          <p className={styles.heroDescription}>在统一 Cargo workspace 中组织三套系统、共享组件与平台适配，通过 feature 和目标配置装配所需能力。</p>
          <dl className={styles.frameworkSummary}>
            <div><dt><span aria-hidden="true">01</span>三套系统，各有职责</dt><dd>ArceOS 提供运行时，StarryOS 实现 Linux 兼容，AxVisor 管理虚拟机。共享基础机制，独立维护系统语义。</dd></div>
            <div><dt><span aria-hidden="true">02</span>共享组件，按需组合</dt><dd>调度、内存、驱动、文件、网络与虚拟化按领域维护，由 Cargo feature 和目标配置选择装配。</dd></div>
            <div><dt><span aria-hidden="true">03</span>平台契约，连接硬件</dt><dd>ax-plat 定义契约，axplat-dyn、somehal 与 someboot 接入启动和硬件，支撑四种架构与实体板卡。</dd></div>
          </dl>
          <div className={styles.heroActions}><a className={layout.primaryButton} href="#component-catalog">浏览全部组件</a><Link to="/docs/architecture/overview">架构文档</Link></div>
          <div className={styles.heroMeta}>
            <span><strong>{entries.length}</strong><span>目录软件包</span></span>
            <span><strong>4</strong><span>逻辑层级</span></span>
          </div>
          </div>
          <figure className={styles.frameworkFigure}>
            <a href={illustration} target="_blank" rel="noopener noreferrer" aria-label="打开完整组件框架图（新窗口）"><ComponentArchitecture className={styles.catalogIllustration} role="img" aria-label="四层组件视图：三套顶层系统、系统接口与集成、共享领域组件、平台与硬件。" /></a>
          </figure>
        </div>
      </header>
      <main>
        <Architecture entries={entries} />
      <section id="component-catalog" className={`container ${styles.main}`} aria-label="全部组件目录">
        <section className={styles.filters} aria-label="筛选目录">
          <div className={styles.filterHeading}>
            <h2>全部组件</h2>
            <label className={styles.search}>
              <span className={styles.srOnly}>搜索名称、简介或标签</span>
              <svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6"><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 5 5" /></svg>
              <input type="search" placeholder="搜索名称、简介或标签…" value={query}
                onChange={(event) => setQuery(event.target.value)} />
            </label>
          </div>
          <div className={styles.chips} aria-label="分类">
            {categories.map((item) => <button type="button" key={item}
              className={styles.chip} aria-pressed={category === item} onClick={() => setCategory(item)}>
              {item} <span>{item === '全部' ? entries.length : entries.filter((entry) => entry.category === item).length}</span>
            </button>)}
          </div>
        </section>
        <div className={styles.results}>
          <p role="status">显示 {visible.length} / {entries.length} 项</p>
          {(query || category !== '全部') && <button type="button" className={styles.reset} onClick={reset}>清除筛选</button>}
        </div>
        <div className={styles.grid}>
          {visible.map((entry) => <article className={styles.card} data-group={entry.group} key={entry.route}>
            <Link className={styles.cardLink} to={entry.route}>
              <div className={styles.cardArt} data-group={entry.group}>
                <Emblem group={entry.group} /><span className={styles.category}>{entry.category}</span>
              </div>
              <div className={styles.cardBody}>
                <h3>{entry.name}</h3>
                <p>{entry.description}</p>
                <div className={styles.tags}>{entry.tags.slice(0, 3).map((tag) => <span key={tag}>{tag}</span>)}</div>
                <span className={styles.featureCount}>{entry.features?.length ? `${entry.features.length} 个 Cargo 功能开关` : '未声明 Cargo 功能开关'}</span>
                <div className={styles.cardFooter}><span className={styles.location}>{entry.location}</span><span className={styles.more}>查看详情</span></div>
              </div>
            </Link>
          </article>)}
        </div>
        {!visible.length && <div className={styles.empty}>
          <h2>没有找到匹配的组件</h2>
          <button type="button" className={layout.primaryButton} onClick={reset}>查看全部</button>
        </div>}
      </section>
      </main>
      </div>
    </Layout>
  );
}
