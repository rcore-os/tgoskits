import React, {useState} from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import useBaseUrl from '@docusaurus/useBaseUrl';
import ComponentArchitecture from '@site/static/images/showcase/component-hierarchy.svg';
import Architecture from './Architecture';
import styles from './styles.module.css';

export const titles = {components: 'Components', apps: 'APPs'};
const descriptions = {
  components: '操作系统与虚拟化平台的基础组件、设备驱动和内存管理模块。',
  apps: 'ArceOS、StarryOS 应用与开发工具。',
};

export function Emblem({group}) {
  const paths = {
    components: <><path d="m12 3 9 5-9 5-9-5 9-5Z" /><path d="m3 12 9 5 9-5M3 16l9 5 9-5" /></>,
    drivers: <><rect x="6" y="6" width="12" height="12" rx="3" /><path d="M9 3v3m6-3v3M9 18v3m6-3v3M3 9h3m-3 6h3m12-6h3m-3 6h3" /><rect x="10" y="10" width="4" height="4" rx="1" /></>,
    memory: <><rect x="3" y="6" width="18" height="12" rx="2" /><path d="M7 10v4m5-4v4m5-4v4M7 18v3m5-3v3m5-3v3" /></>,
    virtualization: <><path d="m12 3 9 5v9l-9 5-9-5V8l9-5Z" /><path d="m3 8 9 5 9-5m-9 5v9M7.5 5.5l9 5" /></>,
    starry: <path d="m12 2 2.6 7.4L22 12l-7.4 2.6L12 22l-2.6-7.4L2 12l7.4-2.6L12 2Z" />,
    arceos: <><path d="m3 20 9-16 9 16M7 13h10" /><path d="m8 20 4-7 4 7" /></>,
  };
  return <span aria-hidden="true" className={styles.emblem} data-group={group}>
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      {paths[group] || <><path d="m8 7-5 5 5 5m8-10 5 5-5 5m-3-12-2 14" /></>}
    </svg>
  </span>;
}

export default function Catalog({catalog: {kind, entries}}) {
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
    <Layout title={titles[kind]} description={descriptions[kind]}>
      <div className={styles.catalog} data-kind={kind}>
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
        <div className={`container ${styles.heroInner}`}>
          <div className={styles.heroCopy}>
          <p className={styles.heroLabel}>TGOSKits · COMPONENT LIBRARY</p>
          <h1>{titles[kind]}</h1>
          <p className={styles.intro}>按需组合，跨系统复用。</p>
          <p className={styles.heroDescription}>在统一 Cargo workspace 中组织三套系统、共享组件与平台适配，通过 feature 和目标配置装配所需能力。</p>
          <dl className={styles.frameworkSummary}>
            <div><dt>三套系统，各有职责</dt><dd>ArceOS 提供模块化运行时，StarryOS 实现 Linux 兼容，Axvisor 管理虚拟机。</dd></div>
            <div><dt>共享组件，按需组合</dt><dd>调度、内存、驱动、文件、网络与虚拟化按领域维护，各系统保留自己的运行策略。</dd></div>
            <div><dt>平台契约，连接硬件</dt><dd>ax-plat、axplat-dyn、somehal 与 someboot 连接四种架构、固件与实体设备。</dd></div>
          </dl>
          <div className={styles.heroActions}><a className="button button--primary" href="#component-catalog">浏览全部组件 ↓</a><Link to="/docs/architecture/overview">架构文档 ↗</Link></div>
          <div className={styles.heroMeta}>
            <span><strong>{entries.length}</strong><span>{kind === 'components' ? '目录软件包' : '应用与工具'}</span></span>
            <span><strong>4</strong><span>逻辑层级</span></span>
          </div>
          </div>
          <figure className={styles.frameworkFigure}>
            <a href={illustration} target="_blank" rel="noopener noreferrer" aria-label="打开完整组件框架图（新窗口）"><ComponentArchitecture className={styles.catalogIllustration} role="img" aria-label="四层组件视图：三套顶层系统、系统接口与集成、共享领域组件、平台与硬件。" /></a>
            <figcaption>上层使用下层能力 · 点击查看完整 SVG</figcaption>
          </figure>
        </div>
      </header>
      <main>
        <Architecture entries={entries} />
      <section id="component-catalog" className={`container ${styles.main}`} aria-label="全部组件目录">
        <section className={styles.filters} aria-label="筛选目录">
          <div className={styles.filterHeading}>
            <h2>{kind === 'components' ? '全部组件' : '应用目录'}</h2>
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
                <div className={styles.cardFooter}><span className={styles.location}>{entry.location}</span><span className={styles.more}>查看详情 <span aria-hidden="true">↗</span></span></div>
              </div>
            </Link>
          </article>)}
        </div>
        {!visible.length && <div className={styles.empty}>
          <h2>没有找到匹配的{kind === 'components' ? '组件' : '应用'}</h2>
          <p>试试其他关键词，或清除筛选查看完整目录。</p>
          <button type="button" className="button button--primary" onClick={reset}>查看全部</button>
        </div>}
      </section>
      </main>
      </div>
    </Layout>
  );
}
