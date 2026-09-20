import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import Emblem from '../components/catalog/Emblem';
import {titles} from '../components/catalog/titles';
import styles from '../components/catalog/styles.module.css';
import layout from '../components/layout/page.module.css';

export default function CatalogDetail({catalog: {kind, entry}}) {
  return (
    <Layout wrapperClassName="site-showcase" title={`${entry.name} · ${titles[kind]}`} description={entry.description}>
      <main className={`container ${styles.detail}`}>
        <Link to={`/${kind}`}>← 返回 {titles[kind]}</Link>
        <header className={styles.detailHeader}>
          <Emblem group={entry.group} />
          <div><p className={styles.eyebrow}>{entry.category}</p><h1>{entry.name}</h1></div>
        </header>
        <p className={styles.detailDescription}>{entry.description}</p>
        <div className={styles.tags}>{entry.tags.map((tag) => <span key={tag}>{tag}</span>)}</div>
        <div className={layout.actions}>
          {entry.readme && <Link className={layout.primaryButton} href={entry.readme}>阅读 README</Link>}
          <Link className={layout.secondaryButton} href={entry.source}>查看源码</Link>
          <Link className={layout.secondaryButton} to={entry.guide}>{kind === 'components' ? '相关架构文档' : '构建与运行指南'}</Link>
        </div>
        <section className={styles.info}>
          <h2>{kind === 'components' ? '组件信息' : '应用信息'}</h2>
          <dl>
            <dt>源码目录</dt><dd><code>{entry.location}</code></dd>
            <dt>所属分类</dt><dd>{entry.category}</dd>
            {entry.version && <><dt>版本</dt><dd>{entry.version}</dd></>}
            {entry.license && <><dt>许可证</dt><dd>{entry.license}</dd></>}
          </dl>
        </section>
        {entry.features?.length > 0 && <section className={styles.info}>
          <h2>Cargo 功能开关</h2>
          <div className={styles.tags}>{entry.features.map((feature) => <code key={feature}>{feature}</code>)}</div>
        </section>}
        {entry.dependencies && <section className={styles.info}>
          <h2>目录内直接依赖</h2>
          <p>包含可选及目标条件声明，不代表当前构建全部启用。</p>
          {entry.dependencies.length ? <ul>{entry.dependencies.map(dependency => <li key={dependency.route}><Link to={dependency.route}>{dependency.name}</Link></li>)}</ul> : <p>未发现指向目录内软件包的直接依赖。</p>}
        </section>}
        {entry.configurations?.length > 0 && <section className={styles.info}>
          <h2>运行配置</h2>
          <ul className={styles.configurations}>{entry.configurations.map((name) =>
            <li key={name}><Link href={`${entry.source}/${name}`}>{name}</Link></li>)}</ul>
        </section>}
      </main>
    </Layout>
  );
}
