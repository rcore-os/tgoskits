import React, {useState} from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
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

function EcosystemArt({kind}) {
  const groups = kind === 'components'
    ? [['components', '基础组件'], ['drivers', '设备驱动'], ['memory', '内存管理'], ['virtualization', '虚拟化']]
    : [['starry', 'StarryOS'], ['arceos', 'ArceOS'], ['tools', '开发工具'], ['components', '应用生态']];
  return <div className={styles.ecosystem} aria-hidden="true">
    <div className={styles.orbit} /><div className={styles.orbitOuter} />
    <svg className={styles.connections} viewBox="0 0 400 220" fill="none">
      <path d="M85 55H155Q200 55 200 110M315 55H245Q200 55 200 110M85 165H155Q200 165 200 110M315 165H245Q200 165 200 110" />
    </svg>
    <div className={styles.core}>
      <div className={styles.coreMark}><Emblem group={kind === 'components' ? 'components' : 'starry'} /></div>
      <strong>TG<span>OS</span>Kits</strong>
    </div>
    {groups.map(([group, label], index) => <div className={styles.node} data-position={index} key={group}>
      <Emblem group={group} /><span>{label}</span>
    </div>)}
  </div>;
}

export default function Catalog({catalog: {kind, entries}}) {
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
      <header className={styles.hero}>
        <div className={`container ${styles.heroInner}`}>
          <div className={styles.heroCopy}>
          <h1>{titles[kind]}</h1>
          <p className={styles.intro}>{descriptions[kind]}</p>
          <div className={styles.heroMeta}>
            <span><strong>{entries.length}</strong><span>{kind === 'components' ? '可复用组件' : '应用与工具'}</span></span>
            <span><strong>{categories.length - 1}</strong><span>{kind === 'components' ? '技术领域' : '运行环境'}</span></span>
          </div>
          </div>
          <EcosystemArt kind={kind} />
        </div>
      </header>
      <main className={`container ${styles.main}`}>
        <section className={styles.filters} aria-label="筛选目录">
          <div className={styles.filterHeading}>
            <h2>{kind === 'components' ? '组件目录' : '应用目录'}</h2>
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
        <div className={styles.masonry}>
          {visible.map((entry) => <article className={styles.card} data-group={entry.group} key={entry.route}>
            <Link className={styles.cardLink} to={entry.route}>
              <div className={styles.cardArt} data-group={entry.group}>
                <Emblem group={entry.group} /><span className={styles.category}>{entry.category}</span>
              </div>
              <div className={styles.cardBody}>
                <h3>{entry.name}</h3>
                <p>{entry.description}</p>
                <div className={styles.tags}>{entry.tags.slice(0, 4).map((tag) => <span key={tag}>{tag}</span>)}</div>
                {entry.features?.length > 0 && <div className={styles.featureList}><span>Cargo 功能</span><div className={styles.tags}>{entry.features.slice(0, 6).map(feature => <code key={feature}>{feature}</code>)}{entry.features.length > 6 && <span>+{entry.features.length - 6}</span>}</div></div>}
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
      </main>
      </div>
    </Layout>
  );
}
