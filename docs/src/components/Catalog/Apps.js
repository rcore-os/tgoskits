import React, {useEffect, useRef, useState} from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import styles from './apps.module.css';

const products = [
  {name: 'postgresql', title: 'PostgreSQL', category: '数据库', type: 'database',
    description: '在 StarryOS 上运行关系数据库，覆盖数据定义、查询、事务与数据完整性验证。',
    features: ['SQL 查询与关联', '事务提交与回滚', '批量写入与数据校验']},
  {name: 'nginx', title: 'Nginx', category: 'Web 服务', type: 'web',
    description: '将 Web 服务带到 StarryOS。通过统一应用入口启动 Nginx，按需执行基础检查、分阶段验证与调试。',
    features: ['独立应用运行入口', '多架构 QEMU 配置', '分阶段验证与调试']},
  {name: 'llama-cpp', title: 'llama.cpp', category: '模型推理', type: 'inference',
    description: '从模型加载到文本生成，在 StarryOS 上运行 llama.cpp，验证 Alpine / musl 环境中的本地推理流程。',
    features: ['量化模型加载', 'CPU 文本生成', 'Alpine / musl 兼容验证']},
];

function ProductArt({type}) {
  const figure = useRef(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const observer = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting) {
        setVisible(true);
        observer.disconnect();
      }
    }, {threshold: 0.25});
    observer.observe(figure.current);
    return () => observer.disconnect();
  }, []);
  return <figure ref={figure} className={styles.art} data-type={type} data-visible={visible}>
    <svg viewBox="0 0 520 340" role="img" aria-label={{database: 'SQL 查询连接数据库表的示意图', web: '浏览器请求连接 Nginx 服务的示意图', inference: '模型从输入到文本生成的示意图'}[type]}>
      <g className={styles.gridLines} stroke="currentColor" opacity=".08">
        {[60, 120, 180, 240, 300, 360, 420, 480].map(x => <path key={x} d={`M${x} 0V340`} />)}
        {[50, 110, 170, 230, 290].map(y => <path key={y} d={`M0 ${y}H520`} />)}
      </g>
      {type === 'database' && <>
        <path className={styles.trace} d="M225 170H285M350 120V90H430M350 220V265H430" />
        <g className={styles.illustration}><rect x="35" y="90" width="190" height="160" rx="14" fill="#172c50" />
          <text x="55" y="122" fill="#aebfff">SQL</text><text x="55" y="168" fill="#fff">SELECT</text><text x="55" y="198" fill="#9ce7d5">FROM records;</text>
          <path d="M55 223h75" stroke="#526d99" strokeWidth="6" strokeLinecap="round" />
          <path d="M285 125v90c0 35 130 35 130 0v-90" fill="#bdcbff" stroke="#597be1" strokeWidth="2" />
          <ellipse cx="350" cy="125" rx="65" ry="25" fill="#e4eaff" stroke="#597be1" strokeWidth="2" />
          <path d="M285 165c0 35 130 35 130 0m-130 40c0 35 130 35 130 0" fill="none" stroke="#597be1" strokeWidth="2" />
        </g>
      </>}
      {type === 'web' && <>
        <path className={styles.trace} d="M220 170H290M410 170h65M455 100v140" />
        <g className={styles.illustration}><rect x="35" y="75" width="185" height="190" rx="14" fill="#fff" stroke="#9bcbbd" strokeWidth="2" />
          <path d="M35 112h185" stroke="#c9dfd7" /><circle cx="55" cy="94" r="4" fill="#51a98c" /><circle cx="71" cy="94" r="4" fill="#a7d3c5" />
          <rect x="55" y="133" width="145" height="40" rx="6" fill="#e0f2ec" /><path d="M55 195h110m-110 20h145m-145 20h80" stroke="#accdbf" strokeWidth="6" strokeLinecap="round" />
          <rect x="290" y="105" width="120" height="130" rx="18" fill="#146e56" /><text x="350" y="167" textAnchor="middle" fill="#fff" fontSize="40">N</text><text x="350" y="205" textAnchor="middle" fill="#bcf3d8">NGINX</text>
        </g>
      </>}
      {type === 'inference' && <>
        <path className={styles.trace} d="M125 170H205M305 170H380M235 110V70M275 230v45" />
        <g className={styles.illustration}><rect x="35" y="130" width="90" height="80" rx="12" fill="#f6e8ff" stroke="#ba93d8" /><text x="80" y="177" textAnchor="middle" fill="#765190">输入</text>
          <rect x="205" y="110" width="100" height="120" rx="22" fill="#67499c" />
          <path d="m255 137 27 16-27 16-27-16 27-16Zm-27 31 27 16 27-16m-54 16 27 16 27-16" fill="none" stroke="#ecdcff" strokeWidth="2" />
          <rect x="380" y="105" width="110" height="130" rx="12" fill="#fff" stroke="#c8a8e0" /><path d="M397 133h62m-62 22h75m-75 22h45m-45 22h60" stroke="#b69ad4" strokeWidth="6" strokeLinecap="round" />
        </g>
      </>}
    </svg>
    <figcaption>应用能力示意</figcaption>
  </figure>;
}

function AppResources({entry}) {
  return <div className={styles.resources}>
    <p>{entry.description}</p>
    <div className={styles.links}>
      <Link to={entry.route}>完整详情 ↗</Link>
      {entry.readme && <Link href={entry.readme}>README ↗</Link>}
      <Link href={entry.source}>源码 ↗</Link>
      <Link to={entry.guide}>构建与运行指南 ↗</Link>
    </div>
    {entry.configurations.length > 0 && <>
      <h4>运行配置</h4>
      <ul>{entry.configurations.map(name => <li key={name}><Link href={`${entry.source}/${name}`}>{name}</Link></li>)}</ul>
    </>}
  </div>;
}

export default function Apps({catalog: {entries}}) {
  const [query, setQuery] = useState('');
  const [category, setCategory] = useState('全部');
  const categories = ['全部', ...new Set(entries.map(entry => entry.category))];
  const visible = entries.filter(entry => (category === '全部' || entry.category === category) &&
    [entry.name, entry.description, ...entry.tags].join(' ').toLowerCase().includes(query.trim().toLowerCase()));
  const featured = products.map(product => ({...product, entry: entries.find(entry => entry.location === `apps/starry/${product.name}`)})).filter(product => product.entry);
  return <Layout wrapperClassName="site-showcase" title="APPs" description="ArceOS 与 StarryOS 上的数据库、Web 服务、模型推理和开发工具。">
    <main className={styles.page}>
      <header className={`container ${styles.hero}`}>
        <div><p className={styles.kicker}>APPs</p><h1>让应用，<br />运行于你的系统。</h1>
          <p className={styles.lead}>ArceOS 与 StarryOS 的应用实践，<br />从 Web 服务、数据库到模型推理。</p>
          <a className={styles.primary} href="#applications">浏览全部应用 <span aria-hidden="true">↓</span></a>
          <p className={styles.count}><strong>{entries.length}</strong> 个应用与工具 · ArceOS / StarryOS</p>
        </div>
        <div className={styles.heroVisual} aria-hidden="true">
          <div className={styles.halo} />
          <div className={styles.platform}>TGOSKits<span>ArceOS · StarryOS</span></div>
          <div className={styles.tile} data-tile="database"><span>SQL</span>数据库</div>
          <div className={styles.tile} data-tile="web"><span>HTTP</span>Web 服务</div>
          <div className={styles.tile} data-tile="inference"><span>LLM</span>模型推理</div>
        </div>
      </header>
      <div className={styles.productNav}><div className="container">{featured.map(product => <a key={product.name} href={`#product-${product.name}`}>{product.category}<span>{product.title} ↗</span></a>)}</div></div>
      <div className="container">
        {featured.map((product, index) => <section key={product.name} id={`product-${product.name}`} className={styles.product}>
          <ProductArt type={product.type} />
          <div className={styles.productCopy}><p className={styles.kicker}>0{index + 1} / {product.category}</p>
            <h2>{product.title}</h2><p>{product.description}</p>
            <ul className={styles.features}>{product.features.map(feature => <li key={feature}>{feature}</li>)}</ul>
            <details className={styles.productDetails}>
              <summary>配置与详情 <span aria-hidden="true">＋</span></summary>
              <AppResources entry={product.entry} />
            </details>
          </div>
        </section>)}
      </div>
      <section id="applications" className={styles.directory}>
        <div className="container"><div className={styles.directoryHeading}><div><p className={styles.kicker}>APPLICATIONS</p><h2>全部应用</h2></div>
          <label><span className={styles.srOnly}>搜索名称、简介或标签</span><input type="search" placeholder="搜索应用…" value={query} onChange={event => setQuery(event.target.value)} /></label>
        </div>
        <div className={styles.categories}>{categories.map(item => <button type="button" key={item} aria-pressed={category === item} onClick={() => setCategory(item)}>{item}</button>)}</div>
        <p role="status">显示 {visible.length} / {entries.length} 项</p>
        <div className={styles.appList}>{visible.map(entry => <details key={entry.route} className={styles.appRow}>
          <summary><strong>{entry.name}</strong><span>{entry.category}</span><span aria-hidden="true">＋</span></summary>
          <AppResources entry={entry} />
        </details>)}</div>
        {!visible.length && <div className={styles.empty}><h3>没有找到匹配的应用</h3><button type="button" onClick={() => {setQuery(''); setCategory('全部');}}>清除筛选</button></div>}
        </div>
      </section>
    </main>
  </Layout>;
}
