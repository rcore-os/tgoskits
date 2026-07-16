import { useEffect, useMemo, useState } from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import './index.css';

/* ── Scroll Reveal Hook ──────────────────────────────────── */
function useScrollReveal() {
  useEffect(() => {
    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (entry.isIntersecting) {
            entry.target.classList.add('is-visible');
          }
        });
      },
      { threshold: 0.12, rootMargin: '0px 0px -40px 0px' }
    );

    document.querySelectorAll('.section-reveal, .card-reveal').forEach((el) => observer.observe(el));
    return () => observer.disconnect();
  }, []);
}

/* ── Icon Library ────────────────────────────────────────── */
const iconLibrary = {
  orbit: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <circle cx="60" cy="60" r="40" className="icon-ring" />
      <circle cx="60" cy="60" r="4" className="icon-core" />
      <path d="M20,60 Q60,10 100,60 Q60,110 20,60" className="icon-orbit" />
    </svg>
  ),
  layers: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <path d="M20 40 L60 20 L100 40 L60 60 Z" className="icon-layer" />
      <path d="M20 70 L60 50 L100 70 L60 90 Z" className="icon-layer" />
      <path d="M20 100 L60 80 L100 100 L60 120 Z" className="icon-layer" />
    </svg>
  ),
  shield: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <path d="M60 10 L100 30 V65 C100 88 83 108 60 112 C37 108 20 88 20 65 V30 Z" className="icon-shield" />
      <path d="M45 55 L55 65 L75 45" className="icon-check" />
    </svg>
  ),
  pulse: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <polyline points="10,70 35,70 50,40 70,90 85,55 110,55" className="icon-pulse" />
    </svg>
  ),
  chip: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <rect x="35" y="35" width="50" height="50" rx="6" className="icon-chip" />
      <g className="icon-chip-pins">
        <line x1="60" y1="10" x2="60" y2="30" />
        <line x1="60" y1="90" x2="60" y2="110" />
        <line x1="10" y1="60" x2="30" y2="60" />
        <line x1="90" y1="60" x2="110" y2="60" />
      </g>
    </svg>
  ),
  server: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <rect x="20" y="30" width="80" height="20" rx="4" className="icon-device" />
      <circle cx="30" cy="40" r="3" className="icon-dot" />
      <circle cx="50" cy="40" r="3" className="icon-dot" />
      <circle cx="70" cy="40" r="3" className="icon-dot" />
      <line x1="20" y1="60" x2="100" y2="60" className="icon-line" />
      <rect x="20" y="70" width="80" height="20" rx="4" className="icon-device" />
      <circle cx="30" cy="80" r="3" className="icon-dot" />
      <circle cx="50" cy="80" r="3" className="icon-dot" />
      <circle cx="70" cy="80" r="3" className="icon-dot" />
    </svg>
  ),
  grid: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <rect x="18" y="18" width="36" height="36" rx="6" className="icon-grid-cell" />
      <rect x="66" y="18" width="36" height="36" rx="6" className="icon-grid-cell" />
      <rect x="18" y="66" width="36" height="36" rx="6" className="icon-grid-cell" />
      <rect x="66" y="66" width="36" height="36" rx="6" className="icon-grid-cell" />
    </svg>
  ),
  plug: (
    <svg viewBox="0 0 120 120" role="presentation" aria-hidden="true">
      <path d="M55 20 L55 50" className="icon-plug-stem" />
      <path d="M65 20 L65 50" className="icon-plug-stem" />
      <rect x="42" y="50" width="36" height="30" rx="6" className="icon-plug-body" />
      <rect x="50" y="80" width="20" height="18" rx="4" className="icon-plug-tip" />
    </svg>
  ),
};

/* ── Component Workspace Diagram ─────────────────────────── */
function ComponentWorkspaceDiagram() {
  const repos = [
    { name: 'axallocator', path: 'components/', tone: 'memory' },
    { name: 'arm_vcpu', path: 'components/', tone: 'virtualization' },
    { name: 'rknpu', path: 'drivers/npu/', tone: 'driver' },
  ];

  const hubItems = [
    { title: '独立组件汇聚', desc: ['60+ 个 subtree 仓库', '内存 · 调度 · 设备 · VFS · 虚拟化'] },
    { title: 'Subtree 同步工具', desc: ['repo.py list / pull / push', '集成验证后同步回上游'] },
    { title: '来源边界清晰', desc: ['repos.csv · target_dir · category'] },
  ];

  return (
    <div className="workspace-diagram" aria-label="Git Subtree component workspace workflow">
      <div className="workspace-diagram__title">Git Subtree 工作流：组件仓库 ↔ 统一工作区 ↔ 上游</div>
      <div className="workspace-diagram__flow">
        <div className="workspace-diagram__repos workspace-diagram__repos--source">
          {repos.map((repo) => (
            <div className={`workspace-diagram__repo workspace-diagram__repo--${repo.tone}`} key={repo.name}>
              <span className="workspace-diagram__repo-mark" aria-hidden="true" />
              <code className="workspace-diagram__repo-name">{repo.name}</code>
              <span className="workspace-diagram__repo-path">{repo.path}</span>
            </div>
          ))}
        </div>
        <div className="workspace-diagram__lane workspace-diagram__lane--pull" aria-hidden="true">
          <span /><span /><span />
        </div>
        <div className="workspace-diagram__hub">
          <strong>TGOSKits</strong>
          <span>统一集成工作区</span>
          <div className="workspace-diagram__hub-divider" />
          {hubItems.map((item, index) => (
            <div className="workspace-diagram__hub-item" key={item.title}>
              <b className={index === 1 ? 'is-alt' : ''}>{item.title}</b>
              {item.desc.map((line) => (<span key={line}>{line}</span>))}
            </div>
          ))}
        </div>
        <div className="workspace-diagram__lane workspace-diagram__lane--push" aria-hidden="true">
          <span /><span /><span />
        </div>
        <div className="workspace-diagram__repos workspace-diagram__repos--upstream">
          {repos.map((repo) => (
            <div className={`workspace-diagram__repo workspace-diagram__repo--${repo.tone}`} key={`${repo.name}-upstream`}>
              <span className="workspace-diagram__repo-mark" aria-hidden="true" />
              <code className="workspace-diagram__repo-name">{repo.name}</code>
              <span className="workspace-diagram__repo-path">独立仓库</span>
            </div>
          ))}
        </div>
      </div>
      <div className="workspace-diagram__command">
        <code>$ python3 scripts/repo/repo.py list</code>
        <span>查看组件仓库映射与同步状态</span>
      </div>
      <div className="workspace-diagram__lineage">
        <span>← 组件仓库</span>
        <strong>集成验证</strong>
        <span>上游仓库 →</span>
      </div>
      <div className="workspace-diagram__tools">
        <code>$ repo.py pull</code>
        <code>$ repo.py push</code>
        <code>repos.csv</code>
      </div>
    </div>
  );
}

/* ── Systems Diagram ─────────────────────────────────────── */
function SystemsDiagram({ systems }) {
  return (
    <div className="systems-diagram" aria-label="Shared components powering ArceOS StarryOS and Axvisor">
      <div className="systems-diagram__cards">
        {systems.map((system) => (
          <article className={`systems-diagram__card ${system.accent}`} key={system.name}>
            <div className="systems-diagram__header"><h3>{system.name}</h3></div>
            <div className="systems-diagram__body">
              <strong>{system.subtitle}</strong>
              <span className="systems-diagram__tag">{system.tag}</span>
              <p>{system.desc}</p>
              <ul>{system.items.map((item) => (<li key={item}>{item}</li>))}</ul>
            </div>
          </article>
        ))}
      </div>
      <div className="systems-diagram__connectors" aria-hidden="true">
        {systems.map((system) => (<span className={system.accent} key={system.name} />))}
      </div>
      <div className="systems-diagram__foundation">
        <strong>共享组件基础层</strong>
        <code>components/ · ax* crates · starry-* · drivers/ · platforms/</code>
      </div>
    </div>
  );
}

/* ── Section Shell ───────────────────────────────────────── */
function SectionShell({ id, className, eyebrow, title, description, children }) {
  return (
    <section className={`section-shell section-reveal ${className || ''}`} id={id}>
      <div className="section-shell__inner">
        <div className="section-header">
          <p className="eyebrow">{eyebrow}</p>
          <h2>{title}</h2>
          <p>{description}</p>
        </div>
        {children}
      </div>
    </section>
  );
}

/* ── Staggered card class helper ─────────────────────────── */
function staggerClass(index) {
  return `card-reveal stagger-${(index % 6) + 1}`;
}

/* ── Hero Banner ─────────────────────────────────────────── */
function HeroBanner() {
  const heroStats = [
    { label: '核心系统', value: '3' },
    { label: '共享组件', value: '190+' },
    { label: '主流架构', value: '4' },
    { label: '统一命令入口', value: 'xtask' },
  ];

  const quickLinks = [
    { label: '项目概览', to: '/docs/introduction/overview' },
    { label: '快速开始', to: '/docs/quickstart/overview' },
    { label: '构建系统', to: '/docs/build/overview' },
    { label: '组件视图', to: '/docs/development/components' },
  ];

  return (
    <section className="hero-banner" id="hero" aria-label="TGOSKits overview banner">
      <svg className="hero-background-svg" viewBox="0 0 1200 800" preserveAspectRatio="xMidYMid slice" aria-hidden="true">
        <rect width="1200" height="800" fill="var(--hero-accent)" opacity="0.08" />
        <path d="M0,100 Q300,50 600,100 T1200,100" stroke="var(--hero-decoration)" strokeWidth="2" fill="none" opacity="0.4" className="hero-wave-top" />
        <path d="M0,120 Q300,80 600,120 T1200,120" stroke="var(--hero-decoration)" strokeWidth="1" fill="none" opacity="0.2" className="hero-wave-top" />
        <circle cx="150" cy="250" r="80" fill="none" stroke="var(--hero-decoration)" strokeWidth="2" opacity="0.2" className="hero-circle-anim" />
        <circle cx="150" cy="250" r="60" fill="none" stroke="var(--hero-decoration)" strokeWidth="1" opacity="0.1" className="hero-circle-anim-delayed" />
        <circle cx="1100" cy="600" r="100" fill="none" stroke="var(--hero-decoration)" strokeWidth="2" opacity="0.15" className="hero-circle-anim-reverse" />
        <line x1="100" y1="650" x2="300" y2="700" stroke="var(--hero-decoration)" strokeWidth="1" opacity="0.3" className="hero-line-anim" />
        <line x1="950" y1="150" x2="1100" y2="200" stroke="var(--hero-decoration)" strokeWidth="1" opacity="0.3" className="hero-line-anim-reverse" />
        <circle cx="600" cy="150" r="4" fill="var(--hero-decoration)" opacity="0.6" className="hero-dot-pulse" />
        <circle cx="200" cy="600" r="3" fill="var(--hero-decoration)" opacity="0.5" className="hero-dot-pulse" />
        <circle cx="1000" cy="400" r="3" fill="var(--hero-decoration)" opacity="0.5" className="hero-dot-pulse-delayed" />
      </svg>

      <div className="hero-content">
        <div className="hero-copy">
          <p className="eyebrow">Operating Systems and Virtualization Workspace</p>
          <h1><span>TGOSKits</span><em>面向系统软件研发的一体化工作区</em></h1>
          <p className="lead">
            ArceOS、StarryOS、Axvisor 三条系统路径共享 190+ Rust crate，
            通过 cargo xtask 统一构建、QEMU 运行和分层验证，在同一仓库内完成从组件开发到系统集成的完整闭环。
          </p>
          <div className="hero-actions">
            <Link className="button button--primary button--hero" to="/docs/introduction/overview">阅读概览</Link>
            <Link className="button button--outline button--hero" to="/docs/quickstart/overview">开始上手</Link>
            <Link className="button button--secondary button--hero" to="https://github.com/rcore-os/tgoskits">GitHub</Link>
          </div>
          <div className="hero-quicklinks">
            {quickLinks.map((link) => (
              <Link key={link.label} className="hero-quicklink" to={link.to}>{link.label}</Link>
            ))}
          </div>
          <div className="hero-stats" role="list">
            {heroStats.map((stat) => (
              <div className="stat" role="listitem" key={stat.label}>
                <span className="stat-value">{stat.value}</span>
                <span className="stat-label">{stat.label}</span>
              </div>
            ))}
          </div>
        </div>
        <div className="hero-visual" aria-hidden="true">
          <HeroTerminal />
        </div>
      </div>

      <svg className="hero-wave-divider" viewBox="0 0 1200 100" preserveAspectRatio="none" aria-hidden="true">
        <path d="M0,20 Q300,0 600,20 T1200,20 L1200,100 L0,100 Z" fill="var(--hero-wave-color)" />
        <path d="M0,30 Q300,10 600,30 T1200,30 L1200,100 L0,100 Z" fill="var(--home-base)" opacity="0.68" />
      </svg>
    </section>
  );
}

function HeroTerminal() {
  const sessions = useMemo(() => [
    {
      os: 'ArceOS',
      command: 'cargo xtask arceos qemu --package arceos-helloworld --arch aarch64',
      output: [
        'Building ArceOS package arceos-helloworld',
        'Launching qemu-system-aarch64 on the virt platform',
        'Booting app: arceos-helloworld',
        'Hello, world!',
      ],
    },
    {
      os: 'StarryOS',
      command: 'cargo xtask starry qemu --arch aarch64',
      output: [
        'Using rootfs-aarch64-alpine.img',
        'Booting StarryOS on qemu-aarch64',
        'Starting init process and user shell',
        'root@starry:~#',
      ],
    },
    {
      os: 'Axvisor',
      command: 'cargo xtask axvisor qemu --arch aarch64',
      output: [
        'Static VM configs are empty.',
        'Now axvisor will entry the shell...',
        'Starting Axvisor on qemu-aarch64',
        'Welcome to AxVisor Shell!',
        'Type \'help\' to see available commands',
        'axvisor:$',
      ],
    },
  ], []);
  const [sessionIndex, setSessionIndex] = useState(0);
  const [typedCount, setTypedCount] = useState(0);
  const [visibleOutputCount, setVisibleOutputCount] = useState(0);
  const session = sessions[sessionIndex];
  const commandDone = typedCount >= session.command.length;
  const outputDone = visibleOutputCount >= session.output.length;

  const handleSessionSelect = (index) => {
    setSessionIndex(index);
    setTypedCount(0);
    setVisibleOutputCount(0);
  };

  useEffect(() => {
    setTypedCount(0);
    setVisibleOutputCount(0);
  }, [sessionIndex]);

  useEffect(() => {
    if (typedCount < session.command.length) {
      const timer = window.setTimeout(() => setTypedCount((count) => count + 1), 28);
      return () => window.clearTimeout(timer);
    }

    if (visibleOutputCount < session.output.length) {
      const timer = window.setTimeout(() => {
        setVisibleOutputCount((count) => count + 1);
      }, visibleOutputCount === 0 ? 420 : 520);
      return () => window.clearTimeout(timer);
    }

    return undefined;
  }, [session.command.length, session.output.length, typedCount, visibleOutputCount]);

  useEffect(() => {
    if (!outputDone) return undefined;

    const timer = window.setTimeout(() => {
      setSessionIndex((index) => (index + 1) % sessions.length);
    }, 1900);
    return () => window.clearTimeout(timer);
  }, [outputDone, sessions.length]);

  return (
    <div className="hero-terminal-container">
      <div className="hero-terminal-header">
        <div className="hero-terminal-buttons">
          <span className="htb htb-close" />
          <span className="htb htb-min" />
          <span className="htb htb-max" />
        </div>
        <span className="hero-terminal-title">workspace shell</span>
      </div>
      <div className="hero-terminal-screen" aria-live="polite">
        <div className="hero-terminal-command">
          <span className="hero-terminal-prompt">$</span>
          <span>{session.command.slice(0, typedCount)}</span>
          {!commandDone && <span className="hero-terminal-cursor" aria-hidden="true" />}
        </div>
        <div className="hero-terminal-output">
          {session.output.slice(0, visibleOutputCount).map((line, index) => (
            <span className={index === session.output.length - 1 ? 'is-success' : undefined} key={line}>{line}</span>
          ))}
          {commandDone && !outputDone && <span className="hero-terminal-cursor" aria-hidden="true" />}
        </div>
      </div>
      <div className="hero-terminal-footer">
        {sessions.map((item, index) => (
          <button
            aria-pressed={index === sessionIndex}
            className={index === sessionIndex ? 'is-active' : undefined}
            key={item.os}
            onClick={() => handleSessionSelect(index)}
            type="button"
          >
            {item.os}
          </button>
        ))}
      </div>
    </div>
  );
}

/* ── Capability Section ──────────────────────────────────── */
function CapabilitySection() {
  const features = [
    { icon: 'orbit', title: '统一构建入口', desc: 'cargo xtask 子命令覆盖构建、运行、测试与发布，单条命令切换系统路径与目标架构。', to: '/docs/build/overview' },
    { icon: 'layers', title: '组件化架构', desc: '内存分配、调度器、文件系统、网络栈等以独立 crate 提取，系统通过组合 crate 而非 fork 衍生。', to: '/docs/development/components' },
    { icon: 'shield', title: 'Rust 内存安全', desc: '内核、驱动与虚拟化路径均基于 Rust 实现，在编译期消除缓冲区溢出与数据竞争等常见系统漏洞。', to: '/docs/architecture/overview' },
    { icon: 'pulse', title: '四架构支持', desc: 'riscv64、aarch64、x86_64、loongarch64 均可通过 xtask 一键构建与 QEMU 运行，接口统一而适配独立。', to: '/docs/introduction/platform' },
    { icon: 'chip', title: '镜像与快照闭环', desc: '从配置生成、交叉编译、镜像打包到 QEMU 启动与快照管理，构建产物可追溯、可复现。', to: '/docs/build/overview' },
    { icon: 'server', title: '分层验证策略', desc: 'Host 侧 cargo test 与 clippy 先行，系统级 QEMU 运行验证跟进，板级回归兜底，验证粒度逐层放大。', to: '/docs/build/overview' },
  ];

  return (
    <SectionShell
      id="capabilities"
      className="section-shell--capabilities"
      eyebrow="Core Capabilities"
      title="面向系统软件工程的核心工程能力"
      description="构建自动化、组件化架构、内存安全、多架构支持、镜像闭环与分层验证——覆盖从开发到发布的完整链路。"
    >
      <div className="capability-board">
        <Link className={`capability-spotlight ${staggerClass(0)}`} to={features[0].to}>
          <div className="capability-spotlight__icon">{iconLibrary[features[0].icon]}</div>
          <span className="capability-kicker">Primary Flow</span>
          <h3>{features[0].title}</h3>
          <p>{features[0].desc}</p>
          <span className="capability-spotlight__link">查看构建系统</span>
        </Link>
        <div className="capability-lanes">
          {features.slice(1).map((feature, i) => (
            <Link className={`capability-lane ${staggerClass(i + 1)}`} key={feature.title} to={feature.to}>
              <span className="capability-lane__index">0{i + 2}</span>
              <div className="feature-icon">{iconLibrary[feature.icon]}</div>
              <div className="capability-lane__body">
                <h3>{feature.title}</h3>
                <p>{feature.desc}</p>
              </div>
            </Link>
          ))}
        </div>
      </div>
    </SectionShell>
  );
}

/* ── Architecture Section ────────────────────────────────── */
function ArchitectureSection() {
  const architectureFlow = [
    { label: '场景入口', items: ['ArceOS examples', 'StarryOS rootfs', 'Axvisor guests', 'board / VM configs'] },
    { label: '系统形态', items: ['ArceOS modular kernel', 'StarryOS Linux-compatible OS', 'Axvisor Type-I hypervisor'] },
    { label: '共享组件', items: ['memory / scheduler', 'fs / net / device', 'VM / vCPU / address space', 'driver core APIs'] },
    { label: '平台与硬件', items: ['axplat crates', 'axhal integration', 'QEMU targets', 'board platforms'] },
  ];

  const sideRails = [
    { title: '构建与配置', items: ['cargo xtask', 'scripts/axbuild', 'platform configs', 'VM configs'] },
    { title: '验证闭环', items: ['clippy / fmt checks', 'ArceOS tests', 'StarryOS test-suit', 'Axvisor QEMU / board tests'] },
  ];

  const notes = [
    { title: '自底向上的依赖约束', desc: '下层 crate 不依赖上层实现，组件层不引用系统层代码，平台层不感知具体系统。依赖方向单一，修改影响可控。' },
    { title: '水平切分的复用边界', desc: '同一层的 crate 通过 trait 或接口抽象解耦，系统通过组合而非继承获取能力，新增系统路径无需修改现有组件。' },
    { title: '验证链路与架构层级对应', desc: 'Host 测试覆盖组件层，QEMU 运行覆盖系统层，板级回归覆盖平台层——验证粒度与架构层级一一映射。' },
  ];

  return (
    <SectionShell
      id="architecture"
      className="section-shell--architecture"
      eyebrow="Architecture"
      title="四层分层，依赖关系自底向上可推导"
      description="场景入口依赖系统形态，系统形态依赖共享组件，共享组件依赖平台抽象——每一层的变更范围可通过依赖图精确界定。"
    >
      <div className="architecture-map">
        <div className="architecture-rail architecture-rail--left">
          <h3>{sideRails[0].title}</h3>
          <ul>{sideRails[0].items.map((item) => (<li key={item}>{item}</li>))}</ul>
        </div>
        <div className="architecture-flow" aria-label="TGOSKits layered architecture">
          {architectureFlow.map((layer, index) => (
            <div className="architecture-layer" key={layer.label} style={{ '--layer-index': index }}>
              <div className="architecture-layer__label">{layer.label}</div>
              <div className="architecture-layer__items">
                {layer.items.map((item) => (<span key={item}>{item}</span>))}
              </div>
            </div>
          ))}
          <div className="architecture-backbone" aria-hidden="true"><span>shared workspace contracts</span></div>
        </div>
        <div className="architecture-rail architecture-rail--right">
          <h3>{sideRails[1].title}</h3>
          <ul>{sideRails[1].items.map((item) => (<li key={item}>{item}</li>))}</ul>
        </div>
      </div>
      <div className="architecture-notes">
        {notes.map((note) => (
          <article className="architecture-note" key={note.title}>
            <h3>{note.title}</h3>
            <p>{note.desc}</p>
          </article>
        ))}
      </div>
    </SectionShell>
  );
}

/* ── Component Workspace Section ─────────────────────────── */
function ComponentWorkspaceSection() {
  return (
    <SectionShell
      id="component-workspace"
      className="section-shell--component-workspace"
      eyebrow="Component Workspace"
      title="Git Subtree 管理的组件同步工作流"
      description="每个组件同时维护独立仓库与工作区副本，通过 repo.py pull/push 双向同步，集成验证通过后才回推上游。"
    >
      <ComponentWorkspaceDiagram />
    </SectionShell>
  );
}

/* ── Systems Section ─────────────────────────────────────── */
function SystemsSection() {
  const systems = [
    { accent: 'accent-arceos', name: 'ArceOS', subtitle: '模块化内核', tag: '组件组合层', desc: '以模块化设计组织内核能力，每个模块对应一个独立 crate，可通过配置裁剪组合出不同形态的系统。', items: ['模块化 crates: axlog, axnet, axfs, axhal …', 'examples 覆盖从 helloworld 到完整系统', 'StarryOS 与 Axvisor 的直接依赖'] },
    { accent: 'accent-starry', name: 'StarryOS', subtitle: 'Linux 兼容 OS', tag: 'POSIX 兼容层', desc: '在 ArceOS 模块基础上实现 Linux 系统调用接口，支持 ELF 加载、进程管理、信号处理与 rootfs 引导。', items: ['syscall 覆盖: 文件 I/O、进程、信号、网络', 'rootfs 构建与用户态程序验证', 'Linux 兼容性 test-suit 回归'] },
    { accent: 'accent-axvisor', name: 'Axvisor', subtitle: 'Type-I Hypervisor', tag: '虚拟化层', desc: '裸机 Hypervisor，管理 VM 生命周期、vCPU 调度、虚拟地址空间与虚拟设备，支持多 Guest 并行运行。', items: ['VM / vCPU 生命周期管理', '虚拟设备: UART、块设备、网络', '多架构 Guest: ArceOS、Linux 等'] },
  ];

  return (
    <SectionShell
      id="systems"
      className="section-shell--systems"
      eyebrow="Systems"
      title="三条系统路径，各自聚焦不同抽象层级"
      description="ArceOS 关注模块组合与平台适配，StarryOS 关注 POSIX 语义与用户态兼容，Axvisor 关注虚拟化抽象与 Guest 管理。"
    >
      <SystemsDiagram systems={systems} />
    </SectionShell>
  );
}

/* ── Workflow Section ────────────────────────────────────── */
function WorkflowSection() {
  const steps = [
    { index: '01', title: '理解仓库分层', desc: '阅读 overview 与 repo 文档，建立组件层、系统层和平台层的心智模型。', to: '/docs/introduction/overview', linkLabel: '项目概览', command: 'docs/introduction/overview' },
    { index: '02', title: '跑通 QEMU 构建', desc: '选择目标系统，用 xtask 一条命令完成编译、镜像生成和虚拟平台运行。', to: '/docs/quickstart/overview', linkLabel: '快速开始', command: 'cargo xtask arceos qemu --package arceos-helloworld --target riscv64gc-unknown-none-elf' },
    { index: '03', title: '参与开发与验证', desc: '进入具体系统指南，了解目录约定、构建命令和验证策略后开始贡献。', to: '/docs/architecture/overview', linkLabel: '架构与验证', command: 'cargo xtask clippy && cargo xtask test' },
  ];

  return (
    <SectionShell
      id="workflow"
      className="section-shell--workflow"
      eyebrow="Getting Started"
      title="三步进入开发：理解 → 构建 → 验证"
      description="先建立分层心智模型，再跑通 QEMU 构建运行，最后深入具体系统的开发与验证流程。"
    >
      <div className="workflow-timeline">
        {steps.map((step, i) => (
          <div className={`workflow-column ${staggerClass(i)}`} key={step.title}>
            <div className="workflow-card__marker"><span className="workflow-index">{step.index}</span></div>
            <article className="workflow-card">
              <div className="workflow-card__content">
                <h3>{step.title}</h3>
                <p>{step.desc}</p>
                <code className="command-pill">{step.command}</code>
                <Link className="workflow-card__link" to={step.to}>{step.linkLabel}</Link>
              </div>
            </article>
          </div>
        ))}
      </div>
    </SectionShell>
  );
}

/* ── Docs Section ────────────────────────────────────────── */
function DocsSection() {
  const docs = [
    { title: '项目介绍', desc: '仓库定位、系统关系、硬件支持矩阵和读者入口。', links: [{ label: '概览', to: '/docs/introduction/overview' }, { label: '架构与平台', to: '/docs/introduction/platform' }] },
    { title: '参考资料', desc: '仓库目录结构、组件清单、构建系统和依赖图谱。', links: [{ label: '仓库结构', to: '/docs/contributing/repo' }, { label: '组件指南', to: '/docs/development/components' }, { label: '构建系统', to: '/docs/build/overview' }] },
    { title: '设计与实现', desc: '分层架构原理、构建链路细节和 Guest 配置方法。', links: [{ label: '架构设计', to: '/docs/architecture/overview' }, { label: '构建流程', to: '/docs/build/overview' }] },
    { title: '系统指南', desc: '按 ArceOS / StarryOS / Axvisor 分别说明目录、命令和验证方式。', links: [{ label: 'ArceOS', to: '/docs/development/arceos' }, { label: 'StarryOS', to: '/docs/development/starryos' }, { label: 'Axvisor', to: '/docs/development/axvisor' }] },
  ];

  return (
    <SectionShell
      id="docs-map"
      className="section-shell--docs"
      eyebrow="Documentation Map"
      title="四个维度组织文档入口"
      description="从项目概览到系统指南，每个维度提供不同粒度的信息，按需跳转即可。"
    >
      <div className="docs-constellation" aria-label="Documentation entry map">
        <svg className="docs-constellation__art" viewBox="0 0 1120 560" preserveAspectRatio="none" aria-hidden="true">
          <path className="docs-constellation__path docs-constellation__path--wide" d="M84 306 C220 252 320 254 430 304 S620 354 744 304 S902 250 1036 298" />
          <path className="docs-constellation__path docs-constellation__path--soft" d="M112 330 C252 366 344 226 496 270 S690 358 846 292 S990 238 1050 262" />
        </svg>
        {docs.map((group, i) => (
          <article className={`docs-node docs-node--${i + 1} ${staggerClass(i)}`} key={group.title}>
            <div className="docs-node__visual" aria-hidden="true">
              <span className="docs-node__ring" />
              <span className="docs-node__number">0{i + 1}</span>
            </div>
            <div className="docs-node__copy">
              <h3>{group.title}</h3>
              <p>{group.desc}</p>
            </div>
            <div className="docs-links">
              {group.links.map((link) => (<Link key={link.label} to={link.to}>{link.label}</Link>))}
            </div>
          </article>
        ))}
      </div>
    </SectionShell>
  );
}

/* ── Quality Section ─────────────────────────────────────── */
function QualitySection() {
  const lanes = [
    { title: 'Host 侧组件验证', desc: '在宿主机上直接执行标准库测试与 clippy 静态检查，秒级反馈，无需交叉编译。', items: ['cargo test -p <crate>', 'cargo xtask clippy', 'cargo xtask test'] },
    { title: 'QEMU 系统级验证', desc: '构建目标系统镜像后在 QEMU 中运行，验证 syscall、进程管理、设备驱动等系统级行为。', items: ['ArceOS example 运行检查', 'StarryOS rootfs + shell 启动', 'Axvisor Guest 引导与交互'] },
    { title: '板级场景回归', desc: '变更涉及平台适配或跨系统共享组件时，在物理板卡上执行端到端回归测试，确认硬件行为一致。', items: ['platforms/* 编译与启动验证', 'VM / Guest 配置兼容性回归', '共享 crate 变更的多系统影响面检查'] },
  ];

  return (
    <SectionShell
      id="quality"
      className="section-shell--quality"
      eyebrow="Verification"
      title="三层验证：组件 → 系统 → 平台，粒度逐层放大"
      description="组件级 cargo test 快速反馈，系统级 QEMU 运行验证功能正确性，平台级板卡回归确认硬件适配完整性。"
    >
      <div className="quality-flow" aria-label="Verification coverage grows from component to system to platform">
        {lanes.map((lane, i) => (
          <article className={`quality-stage quality-stage--${i + 1} ${staggerClass(i)}`} key={lane.title} style={{ '--quality-level': i }}>
            <div className="quality-step__rail">
              <span className="quality-level">0{i + 1}</span>
              <span className="quality-status">{i === 0 ? 'Local' : i === 1 ? 'System' : 'Scenario'}</span>
            </div>
            <div className="quality-step__body">
              <h3>{lane.title}</h3>
              <p>{lane.desc}</p>
              <ul>{lane.items.map((item) => (<li key={item}>{item}</li>))}</ul>
            </div>
          </article>
        ))}
      </div>
    </SectionShell>
  );
}

/* ── Platform Section ────────────────────────────────────── */
function PlatformSection() {
  const platformGroups = [
    { arch: 'aarch64', cssClass: 'aarch64', label: 'ARMv8 (AArch64)', targets: [{ name: 'QEMU virt', desc: '虚拟平台仿真', type: 'qemu' }, { name: 'Raspberry Pi', desc: '树莓派板卡', type: 'board' }, { name: 'Phytium Pi', desc: '飞腾派板卡', type: 'board' }, { name: 'BSTA1000B', desc: 'BSTA 板卡', type: 'board' }] },
    { arch: 'riscv64', cssClass: 'riscv64', label: 'RISC-V 64', targets: [{ name: 'QEMU virt', desc: '虚拟平台仿真', type: 'qemu' }, { name: 'VisionFive 2', desc: 'StarFive 板卡', type: 'board' }] },
    { arch: 'x86_64', cssClass: 'x8664', label: 'x86-64', targets: [{ name: 'PC (QEMU)', desc: 'x86 PC 平台', type: 'qemu' }] },
    { arch: 'loongarch64', cssClass: 'loongarch64', label: 'LoongArch 64', targets: [{ name: 'QEMU virt', desc: '虚拟平台仿真', type: 'qemu' }] },
  ];

  return (
    <SectionShell
      id="platforms"
      className="section-shell--platforms"
      eyebrow="Platform Matrix"
      title="四种 CPU 架构，虚拟平台与物理板卡并行支持"
      description="每种架构通过 axplat crate 抽象平台差异，板卡适配与 QEMU 仿真共用同一接口层。"
    >
      <div className="platform-lanes">
        {platformGroups.map((group) => (
          <div className={`platform-lane platform-lane--${group.cssClass}`} key={group.arch}>
            <div className="platform-lane__arch">
              <span className="platform-arch-badge" aria-label={group.label}>{group.arch}</span>
            </div>
            <div className="platform-lane__rail">
              {group.targets.map((target) => (
                <div className={`platform-chip platform-chip--${target.type}`} key={target.name}>
                  <span className="platform-chip__type" aria-hidden="true" />
                  <span className="platform-chip__name">{target.name}</span>
                  <span className="platform-chip__desc">{target.desc}</span>
                </div>
              ))}
            </div>
            <div className="platform-lane__coverage">
              <strong>{group.targets.length}</strong>
              <span>{group.targets.some((t) => t.type === 'board') ? 'QEMU + board' : 'QEMU'}</span>
            </div>
          </div>
        ))}
        <div className="platform-lanes__action">
          <Link className="platform-lanes__link" to="/docs/introduction/platform">查看完整硬件支持</Link>
        </div>
      </div>
    </SectionShell>
  );
}

/* ── Driver Section ──────────────────────────────────────── */
function DriverSection() {
  const driverCategories = [
    { icon: 'server', title: '块设备驱动', desc: 'SD/MMC 存储支持', cssClass: 'blk', items: ['sdhci-host', 'dwmmc-host', 'sdmmc-protocol'] },
    { icon: 'chip', title: 'NPU 驱动', desc: '神经网络加速', cssClass: 'npu', items: ['rockchip-npu'] },
    { icon: 'layers', title: 'PCI 总线驱动', desc: 'PCIe 控制器适配', cssClass: 'pci', items: ['rk3588-pci'] },
    { icon: 'grid', title: 'SoC 平台驱动', desc: '片上系统外设', cssClass: 'soc', items: ['rockchip (GPIO, clk, reset)'] },
  ];

  const driverSubsystems = [
    { name: 'rdif-block', label: '块设备' },
    { name: 'rd-net', label: '网络' },
    { name: 'rdif-display', label: '显示' },
    { name: 'rdif-input', label: '输入' },
    { name: 'rdif-vsock', label: '虚拟 Socket' },
    { name: 'rdrive', label: '设备注册' },
  ];

  return (
    <SectionShell
      id="drivers"
      className="section-shell--drivers"
      eyebrow="Driver Ecosystem"
      title="驱动核心逻辑与 OS 依赖完全解耦"
      description="Driver Core 只包含硬件操作逻辑，OS Glue 层适配不同内核的内存分配与调度接口，同一驱动无需修改即可在 ArceOS、StarryOS 和 Axvisor 中使用。"
    >
      <div className="split-layout split-layout--drivers">
        <div className="driver-device-grid">
          <h3>具体设备驱动</h3>
          <p className="driver-subtitle">drivers/ 目录下的硬件驱动实现</p>
          <div className="driver-device-cards">
            {driverCategories.map((cat) => (
              <div className={`driver-device-card driver-device-card--${cat.cssClass}`} key={cat.title}>
                <div className="feature-icon">{iconLibrary[cat.icon]}</div>
                <div className="driver-device-card__body">
                  <h4>{cat.title}</h4>
                  <p>{cat.desc}</p>
                  <div className="driver-device-tags">
                    {cat.items.map((item) => (<span className="driver-tag" key={item}>{item}</span>))}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </div>
        <div className="driver-subsystem-panel">
          <h3>驱动子系统抽象</h3>
          <p className="driver-subtitle">rdrive 与 RDIF 提供的通用驱动能力层</p>
          <div className="driver-subsystem-grid">
            {driverSubsystems.map((sub) => (
              <div className="driver-subsystem-chip" key={sub.name}>
                <code>{sub.name}</code>
                <span>{sub.label}</span>
              </div>
            ))}
          </div>
          <div className="driver-framework-note">
            <h4>跨内核驱动框架</h4>
            <p>基于 Driver Core → Capability Boundary → OS Glue → Runtime 四层分层模型，将驱动核心逻辑与 OS 依赖解耦，通过 mmio-api / dma-api / IRQ 契约实现跨系统复用。</p>
          </div>
        </div>
      </div>
    </SectionShell>
  );
}

/* ── Home Page ───────────────────────────────────────────── */
export default function Home() {
  const { siteConfig } = useDocusaurusContext();
  useScrollReveal();

  return (
    <Layout title={siteConfig.title} description={siteConfig.tagline} wrapperClassName="home">
      <HeroBanner />
      <CapabilitySection />
      <ArchitectureSection />
      <ComponentWorkspaceSection />
      <SystemsSection />
      <PlatformSection />
      <DriverSection />
      <QualitySection />
      <DocsSection />
      <WorkflowSection />
    </Layout>
  );
}
