import useVisualHeight from '../hooks/useVisualHeight';
import layout from '../components/layout/page.module.css';
import React, {useEffect, useRef, useState} from 'react';
import Layout from '@theme/Layout';
import {usePluginData} from '@docusaurus/useGlobalData';
import Link from '@docusaurus/Link';
import styles from './apps.module.css';

const products = [
  {name: 'postgresql', title: 'PostgreSQL', category: '数据库', type: 'database',
    description: '在 StarryOS 上运行关系数据库，覆盖数据定义、查询、事务与数据完整性验证。',
    features: ['SQL 查询与关联', '事务提交与回滚', '批量写入与数据校验'], preparation: '通过应用运行器准备 PostgreSQL 环境，初始化数据库并执行结构化 SQL 工作负载。'},
  {name: 'nginx', title: 'Nginx', category: 'Web 服务', type: 'web',
    description: '将 Web 服务带到 StarryOS。通过统一应用入口启动 Nginx，按需执行基础检查、分阶段验证与调试。',
    features: ['独立应用运行入口', '多架构 QEMU 配置', '分阶段验证与调试'], preparation: '默认 QEMU 配置执行基础功能检查，分阶段与调试模式使用各自的运行配置。'},
  {name: 'llama-cpp', title: 'llama.cpp', category: '模型推理', type: 'inference',
    description: '从模型加载到文本生成，在 StarryOS 上运行 llama.cpp，验证 Alpine / musl 环境中的本地推理流程。',
    features: ['量化模型加载', 'CPU 文本生成', 'Alpine / musl 兼容验证'], preparation: '运行前需将 llama-cli 和 SmolLM2-135M Q4_0 模型注入 Alpine rootfs。案例覆盖模型加载和 token 生成。'},
  {name: 'redis', title: 'Redis', category: '缓存与存储', type: 'cache',
    description: '在 StarryOS 中运行 Redis 数据服务，验证键值操作及持久化相关行为，连接网络服务、文件 I/O 与用户态运行环境。',
    features: ['Redis 功能检查', '独立 AOF 追加写验证', '显式压力测试配置'],
    preparation: '预构建步骤将 Redis、运行库和测试脚本装入应用 overlay。AOF 与压力场景通过独立配置启动。'},
  {name: 'ffmpeg', title: 'FFmpeg', category: '音视频处理', type: 'media',
    description: '将音视频处理工具运行在 StarryOS 上，以 FFmpeg 应用镜像和测试脚本验证多媒体程序所需的文件、内存与线程能力。',
    features: ['独立 FFmpeg 应用镜像', '音视频处理工具链', 'QEMU 应用脚本验证'],
    preparation: '使用 FFmpeg 专用 rootfs 和应用配置，通过 /usr/bin/test_ffmpeg.sh 执行案例验证。具体媒体格式与检查项以脚本为准。'},
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
    <svg viewBox="0 0 520 340" role="img" aria-label={{database: 'SQL 查询连接数据库表的示意图', web: '浏览器请求连接 Nginx 服务的示意图', inference: '模型从输入到文本生成的示意图', cache: 'Redis 键值存储与持久化示意图', media: 'FFmpeg 输入媒体、处理与输出示意图'}[type]}>
      <g stroke="currentColor" opacity=".08">
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
      {type === 'cache' && <g className={styles.illustration}>
        <rect x="55" y="75" width="190" height="190" rx="16" fill="#fff" stroke="#dbabb0" />
        <text x="80" y="113" fill="#a53b4b">Redis</text>
        {[0, 1, 2].map(row => <g key={row}><rect x="78" y={133 + row * 36} width="56" height="23" rx="4" fill="#f5dce0" /><rect x="145" y={133 + row * 36} width="75" height="23" rx="4" fill="#f9edf0" /></g>)}
        <path className={styles.trace} d="M245 170h65" />
        <rect x="310" y="116" width="150" height="108" rx="14" fill="#ac4658" /><text x="385" y="163" textAnchor="middle" fill="#fff">AOF</text><text x="385" y="198" textAnchor="middle" fill="#fff">持久化</text>
      </g>}
      {type === 'media' && <g className={styles.illustration}>
        <rect x="35" y="113" width="125" height="114" rx="14" fill="#e1f0f7" stroke="#8dbace" /><path d="m83 145 35 25-35 25Z" fill="#508caa" />
        <path className={styles.trace} d="M160 170h35m130 0h35" />
        <rect x="195" y="90" width="130" height="160" rx="18" fill="#326580" /><text x="260" y="155" textAnchor="middle" fill="#fff">FFmpeg</text><text x="260" y="193" textAnchor="middle" fill="#fff">媒体处理</text>
        <rect x="360" y="113" width="125" height="114" rx="14" fill="#fff" stroke="#8dbace" /><path d="M379 174h12l8-25 12 48 12-35 9 12h31" fill="none" stroke="#508caa" strokeWidth="3" />
      </g>}
    </svg>
  </figure>;
}

function AppResources({entry, product}) {
  return <div className={styles.resources}>
    <p>{product.preparation}</p>
    <div className={styles.links}>
      <Link to={entry.route}>完整详情</Link>
      {entry.readme && <Link href={entry.readme}>README</Link>}
      <Link href={entry.source}>源码</Link>
      <Link to={entry.guide}>构建与运行指南</Link>
    </div>
    {entry.configurations.length > 0 && <>
      <h4>运行配置</h4>
      <ul>{entry.configurations.map(name => <li key={name}><Link href={`${entry.source}/${name}`}>{name}</Link></li>)}</ul>
    </>}
  </div>;
}

export default function AppsPage() {
  const pageRef = useVisualHeight();
  const {apps: entries} = usePluginData('tgoskits-catalog');
  const featured = products.map(product => ({...product, entry: entries.find(entry => entry.location === `apps/starry/${product.name}`)})).filter(product => product.entry);
  return <Layout wrapperClassName="site-showcase" title="Showcase" description="TGOSKits 大型应用实践：PostgreSQL、Redis、Nginx、llama.cpp 与 FFmpeg。">
    <main className={styles.page} ref={pageRef}>
      <header className={styles.hero}><div className={`container ${styles.heroInner}`} data-visual-pair>
        <div className={styles.copy} data-visual-copy><p className={styles.eyebrow}>Showcase</p><h1 className={layout.heroTitle}>从系统能力，到应用实践。</h1>
          <p className={styles.description}>来自源码 apps/ 的大型应用案例，展示 StarryOS 在数据库、Web 服务、模型推理与音视频处理中的应用实践。</p>
          <dl className={layout.featureList}>
            <div><dt><span aria-hidden="true">01</span>数据与在线服务</dt><dd>PostgreSQL、Redis 与 Nginx 展示数据库、缓存和 Web 服务在 StarryOS 上的运行方式，覆盖文件、网络与进程协作。</dd></div>
            <div><dt><span aria-hidden="true">02</span>推理与多媒体</dt><dd>llama.cpp 从量化模型加载走到文本生成，FFmpeg 连接媒体输入与处理流程，体现用户态程序对内存、线程和 I/O 的综合需求。</dd></div>
            <div><dt><span aria-hidden="true">03</span>可追溯的运行案例</dt><dd>每个案例对应 apps/ 中的源码、准备步骤和运行配置，保留独立验证入口；具体功能范围与运行条件以对应案例为准。</dd></div>
          </dl>
          <a className={styles.primaryButton} href="#applications">浏览应用案例</a>
          <p className={styles.count}><strong>{featured.length}</strong> 个应用案例 · StarryOS</p>
        </div>
        <div className={styles.heroVisual} aria-hidden="true">
          <div className={styles.halo} />
          <div className={styles.platform}>TGOSKits<span>ArceOS · StarryOS</span></div>
          <div className={styles.tile} data-tile="database"><span>SQL</span>数据库</div>
          <div className={styles.tile} data-tile="web"><span>HTTP</span>Web 服务</div>
          <div className={styles.tile} data-tile="inference"><span>LLM</span>模型推理</div>
        </div>
      </div>
      <nav aria-label="案例页面导航" className={styles.productNav}><div className="container">{featured.map(product => <a key={product.name} href={`#product-${product.name}`}>{product.category}<span>{product.title}</span></a>)}</div></nav>
      </header>
      <div id="applications" className="container">
        {featured.map((product, index) => <section key={product.name} id={`product-${product.name}`} className={styles.product} data-visual-pair>
          <ProductArt type={product.type} />
          <div className={styles.copy} data-visual-copy><p className={styles.eyebrow}>0{index + 1} / {product.category}</p>
            <h2 className={layout.sectionTitle}>{product.title}</h2><p className={layout.description}>{product.description}</p>
            <ul className={styles.features}>{product.features.map(feature => <li key={feature}>{feature}</li>)}</ul>
            <details className={styles.productDetails}>
              <summary>运行与源码 <span aria-hidden="true">＋</span></summary>
              <AppResources entry={product.entry} product={product} />
            </details>
          </div>
        </section>)}
      </div>
    </main>
  </Layout>;
}
