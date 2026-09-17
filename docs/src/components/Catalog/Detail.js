import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import {Emblem, titles} from './index';
import styles from './styles.module.css';

export default function Detail({catalog: {kind, entry}}) {
  return (
    <Layout title={`${entry.name} · ${titles[kind]}`} description={entry.description}>
      <main className={`container ${styles.detail}`}>
        <Link to={`/${kind}`}>← 返回 {titles[kind]}</Link>
        <header className={styles.detailHeader}>
          <Emblem group={entry.group} />
          <div><p className={styles.eyebrow}>{entry.category}</p><h1>{entry.name}</h1></div>
        </header>
        <p className={styles.detailDescription}>{entry.description}</p>
        <div className={styles.tags}>{entry.tags.map((tag) => <span key={tag}>{tag}</span>)}</div>
        <div className={styles.actions}>
          {entry.readme && <Link className="button button--primary" href={entry.readme}>阅读 README</Link>}
          <Link className="button button--secondary" href={entry.source}>查看源码</Link>
          <Link className="button button--secondary" to={entry.guide}>{kind === 'components' ? '相关架构文档' : '构建与运行指南'}</Link>
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
          <p className={styles.note}>各功能的含义及组合要求请参阅组件 README 和 Cargo.toml。</p>
        </section>}
        {entry.configurations?.length > 0 && <section className={styles.info}>
          <h2>运行配置</h2>
          <p>选择配置查看对应的运行参数；具体准备步骤请参阅应用 README。</p>
          <ul className={styles.configurations}>{entry.configurations.map((name) =>
            <li key={name}><Link href={`${entry.source}/${name}`}>{name}</Link></li>)}</ul>
        </section>}
      </main>
    </Layout>
  );
}
