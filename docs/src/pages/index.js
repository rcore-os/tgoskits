import useVisualHeight from '../hooks/useVisualHeight';
import { useEffect, useMemo, useState } from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import layout from '../components/layout/page.module.css';
import './index.css';

// Verified against Cargo metadata and scripts/repo/repos.csv on 2026-09-17.
// Workspace members include applications, tests and build tools, unlike the component catalog.
const workspaceFacts = { packages: 193, subtreeMappings: 41, existingSubtreeTargets: 39 };

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
    { name: 'axcpu', path: 'components/axcpu', tone: 'memory' },
    { name: 'arm_vgic', path: 'virtualization/arm_vgic', tone: 'virtualization' },
    { name: 'rockchip-npu', path: 'drivers/npu/rockchip-npu', tone: 'driver' },
  ];

  const hubItems = [
    { title: '外部仓库汇聚', desc: [`${workspaceFacts.subtreeMappings} 条映射记录`, `${workspaceFacts.existingSubtreeTargets} 个目标目录存在`] },
    { title: 'Subtree 同步工具', desc: ['repo.py list / pull / push', '集成验证后同步回上游'] },
    { title: '来源边界清晰', desc: ['repos.csv · target_dir · category'] },
  ];

  return (
    <div className="workspace-diagram" aria-label="Git Subtree component workspace workflow">
      <div className="workspace-diagram__title">{workspaceFacts.subtreeMappings} 条 Git Subtree 映射记录：外部仓库 ↔ 统一工作区 ↔ 上游</div>
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
function SystemThumbnail({system}) {
  return <svg className="systems-diagram__art" viewBox="0 0 480 300" role="img" aria-label={`${system.name} 架构概览：${system.layers.map(layer => layer.join('、')).join('，')}`}>
    <title>{system.name} 架构概览</title>
    <g className="systems-diagram__connections">
      <path d="M240 76v30m-5-6 5 6 5-6M240 158v30m-5-6 5 6 5-6M240 240v22" />
    </g>
    {system.layers.map((layer, row) => {
      const gap = 12;
      const width = (400 - gap * (layer.length - 1)) / layer.length;
      return <g key={row} className={`systems-diagram__layer systems-diagram__layer--${row}`}>
        {layer.map((label, column) => <g key={label}>
          <rect x={40 + column * (width + gap)} y={24 + row * 82} width={width} height="52" rx="10" />
          <text x={40 + column * (width + gap) + width / 2} y={56 + row * 82} textAnchor="middle">{label}</text>
        </g>)}
      </g>;
    })}
    <text className="systems-diagram__foundation" x="240" y="283" textAnchor="middle">{system.foundation}</text>
  </svg>;
}

function SystemsDiagram({ systems }) {
  return (
    <div className="systems-diagram" aria-label="三套系统架构概览">
      <div className="systems-diagram__cards">
        {systems.map((system) => (
          <article className={`systems-diagram__card ${system.accent}`} key={system.name}>
            <Link className="systems-diagram__visual" to={`/oss#${system.id}`} aria-label={`${system.name} 完整架构`}><SystemThumbnail system={system} /></Link>
            <div className="systems-diagram__body">
              <p className="systems-diagram__type">{system.subtitle}</p>
              <h3><Link to={`/oss#${system.id}`}>{system.name}</Link></h3>
              <p>{system.desc}</p>
              <ul>{system.items.map((item) => (<li key={item}>{item}</li>))}</ul>
              <Link className="systems-diagram__link" to={`/oss#${system.id}`}>系统架构</Link>
            </div>
          </article>
        ))}
      </div>
    </div>
  );
}

/* ── Section Shell ───────────────────────────────────────── */
function SectionShell({ id, className, eyebrow, title, description, children }) {
  return (
    <section className={`section-shell section-reveal ${layout.section} ${className || ''}`} id={id}>
      <div className={`section-shell__inner ${layout.container}`}>
        <div className="section-header">
          <p className={`eyebrow ${layout.eyebrow}`}>{eyebrow}</p>
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
    { label: '工作区成员', value: workspaceFacts.packages },
    { label: '目标架构', value: '4' },
    { label: '统一命令入口', value: 'xtask' },
  ];

  const quickLinks = [
    { label: '项目概览', to: '/docs/introduction/overview' },
    { label: '快速开始', to: '/docs/quickstart/overview' },
    { label: '构建系统', to: '/docs/build/overview' },
    { label: '架构视图', to: '/docs/architecture/overview' },
    { label: 'Components', to: '/components' },
    { label: 'Showcase', to: '/apps' },
  ];

  return (
    <section className={`hero-banner ${layout.hero}`} id="hero" aria-label="TGOSKits overview banner">
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

      <div className={`hero-content ${layout.container} ${layout.split} ${layout.heroInner}`}>
        <div className={`hero-copy ${layout.copy}`}>
          <p className={`eyebrow ${layout.eyebrow}`}>Operating Systems and Virtualization Workspace</p>
          <h1><span>TGOSKits</span><em>面向系统软件研发的一体化工作区</em></h1>
          <p className="lead">
            ArceOS、StarryOS、Axvisor 三套系统与它们共享的组件、内存、驱动、虚拟化和平台实现，位于同一个包含 {workspaceFacts.packages} 个成员的 Cargo workspace 中。
            cargo xtask 统一承担配置解析、构建、镜像生成、QEMU 与板卡运行以及分层验证，使同一处组件改动可以在多个系统与目标架构上直接复现，而不需要为每套系统维护独立的构建脚本。
          </p>
          <div className={`hero-actions ${layout.actions}`}>
            <Link className={layout.primaryButton} to="/docs/introduction/overview">阅读概览</Link>
            <Link className={layout.secondaryButton} to="/docs/quickstart/overview">开始上手</Link>
            <Link className={layout.secondaryButton} to="https://github.com/rcore-os/tgoskits">GitHub</Link>
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
        <div className="hero-visual">
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
        'root@starry:/root #',
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
  const [reducedMotion, setReducedMotion] = useState(false);
  useEffect(() => {
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const update = () => setReducedMotion(preference.matches);
    update();
    preference.addEventListener('change', update);
    return () => preference.removeEventListener('change', update);
  }, []);
  const [sessionIndex, setSessionIndex] = useState(0);
  const [typedCount, setTypedCount] = useState(0);
  const [visibleOutputCount, setVisibleOutputCount] = useState(0);
  const session = sessions[sessionIndex];
  const commandDone = reducedMotion || typedCount >= session.command.length;
  const outputDone = reducedMotion || visibleOutputCount >= session.output.length;

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
    if (reducedMotion) return undefined;
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
  }, [reducedMotion, session.command.length, session.output.length, typedCount, visibleOutputCount]);

  useEffect(() => {
    if (reducedMotion || !outputDone) return undefined;

    const timer = window.setTimeout(() => {
      setSessionIndex((index) => (index + 1) % sessions.length);
    }, 1900);
    return () => window.clearTimeout(timer);
  }, [reducedMotion, outputDone, sessions.length]);

  return (
    <div className="hero-terminal-container">
      <div className="hero-terminal-header">
        <div className="hero-terminal-buttons">
          <span className="htb htb-close" />
          <span className="htb htb-min" />
          <span className="htb htb-max" />
        </div>
        <span className="hero-terminal-title">运行流程示意 · 非实时日志</span>
      </div>
      <div className="hero-terminal-screen" aria-live="polite">
        <div className="hero-terminal-command">
          <span className="hero-terminal-prompt">$</span>
          <span>{reducedMotion ? session.command : session.command.slice(0, typedCount)}</span>
          {!commandDone && <span className="hero-terminal-cursor" aria-hidden="true" />}
        </div>
        <div className="hero-terminal-output">
          {session.output.slice(0, reducedMotion ? session.output.length : visibleOutputCount).map((line, index) => (
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
function CapabilityIllustration() {
  const domains = [
    { name: 'components/', detail: '任务 · 调度 · CPU 抽象 · 基础工具' },
    { name: 'memory/', detail: '内存分配 · 地址空间 · 页表 · DMA / MMIO' },
    { name: 'drivers/', detail: '块设备 · 网络设备 · USB · 中断与设备接口' },
    { name: 'fs/', detail: 'VFS · 文件系统 · 页缓存与块设备集成' },
    { name: 'net/', detail: '网络协议栈 · Socket · 网络设备适配' },
    { name: 'virtualization/', detail: 'VM · 虚拟设备 · 客户机地址空间' },
    { name: 'platforms/', detail: '平台契约 · 启动与固件 · 硬件资源' },
  ];
  const systems = [
    { className: 'arceos', name: 'ArceOS', detail: '模块化运行时' },
    { className: 'starry', name: 'StarryOS', detail: 'Linux 用户态兼容' },
    { className: 'axvisor', name: 'Axvisor', detail: 'Type-I Hypervisor' },
  ];
  const architectures = ['AArch64', 'RISC-V', 'x86_64', 'LoongArch'];
  return (
    <figure className="capability-illustration card-reveal stagger-1">
      <div className="capability-art-scroll" tabIndex={0} role="region" aria-label="系统与七类组件能力图，窄屏可横向滚动">
        <svg aria-labelledby="capability-art-title capability-art-description" className="capability-art" role="img" viewBox="0 0 1440 860">
          <title id="capability-art-title">TGOSKits 系统与组件能力全景</title>
          <desc id="capability-art-description">左侧七类领域组件汇聚至中央 TGOSKits 工作区，右侧连接 ArceOS、StarryOS 与 Axvisor。上方 cargo xtask 编排构建与验证，下方列出四种目标架构。连线表示能力组合，不是逐包依赖图。</desc>
          <rect className="capability-art__frame" x="1" y="1" width="1438" height="858" rx="28" />
          <g className="capability-art__connections">
            <path d="M785 136V245" />
            {domains.map((domain, index) => <path key={domain.name} d={`M450 ${36 + index * 112 + 50}H495C550 ${36 + index * 112 + 50} 550 ${315 + index * 30} 600 ${315 + index * 30}`} />)}
            {systems.map((system, index) => <path key={system.name} d={`M970 ${335 + index * 70}C1030 ${335 + index * 70} 1040 ${235 + index * 190} 1110 ${235 + index * 190}`} />)}
            <path d="M785 575V714H1285M685 714H785" />
            {[685, 885, 1085, 1285].map(x => <path key={x} d={`M${x} 714V764`} />)}
          </g>
          <g className="capability-art__xtask">
            <rect x="635" y="52" width="300" height="84" rx="18" />
            <text className="capability-art__overline" x="785" y="82" textAnchor="middle">UNIFIED ORCHESTRATION</text>
            <text className="capability-art__title" x="785" y="115" textAnchor="middle">cargo xtask</text>
          </g>
          {domains.map((domain, index) => {
            const y = 36 + index * 112;
            return <g className="capability-art__domain" key={domain.name}>
              <rect x="40" y={y} width="410" height="100" rx="16" />
              <path d={`M60 ${y + 23}h13l6 7h24v22H60Z`} />
              <text className="capability-art__node-title" x="120" y={y + 39}>{domain.name}</text>
              <text className="capability-art__node-copy" x="60" y={y + 78}>{domain.detail}</text>
            </g>;
          })}
          <g className="capability-art__workspace">
            <rect x="600" y="245" width="370" height="330" rx="28" />
            <text className="capability-art__overline" x="785" y="290" textAnchor="middle">CARGO WORKSPACE</text>
            <text className="capability-art__hub-title" x="785" y="360" textAnchor="middle">TGOSKits</text>
            <text className="capability-art__node-copy" x="785" y="400" textAnchor="middle">共享组件 · 系统集成</text>
            <line x1="645" x2="925" y1="428" y2="428" />
            <text className="capability-art__node-copy" x="785" y="477" textAnchor="middle">配置 · 构建 · 运行 · 验证</text>
            <text className="capability-art__node-copy" x="785" y="521" textAnchor="middle">明确接口边界，按需组合能力</text>
          </g>
          {systems.map((system, index) => {
            const y = 190 + index * 190;
            return <g className={`capability-art__system capability-art__system--${system.className}`} key={system.name}>
              <rect x="1110" y={y} width="280" height="90" rx="18" />
              <text className="capability-art__system-title" x="1140" y={y + 38}>{system.name}</text>
              <text className="capability-art__node-copy" x="1140" y={y + 67}>{system.detail}</text>
            </g>;
          })}
          <text className="capability-art__overline" x="1085" y="700" textAnchor="middle">ARCHITECTURE TARGETS</text>
          {architectures.map((architecture, index) => <g className="capability-art__arch" key={architecture}>
            <rect x={600 + index * 200} y="764" width="170" height="57" rx="14" />
            <text x={685 + index * 200} y="800" textAnchor="middle">{architecture}</text>
          </g>)}
        </svg>
      </div>
    </figure>
  );
}

function CapabilitySection() {
  const features = [
    { icon: 'orbit', title: '统一工程编排', desc: 'cargo xtask 是三套系统共用的命令入口，覆盖配置生成、构建、镜像处理、QEMU 与板卡运行以及分层测试，同一条命令在不同目标架构间保持相同的参数约定与判定方式。', to: '/docs/build/overview' },
    { icon: 'grid', title: '内存基础能力', desc: 'memory/ 收录分配器、地址类型、memory set、多架构页表以及 DMA 与 MMIO API，把物理地址转换和资源映射的差异收敛在明确的能力边界内。', to: '/docs/architecture/memory/overview' },
    { icon: 'layers', title: '任务与调度原语', desc: 'ax-task、axsched、cpumask、ax-lazyinit 与 timer_list 提供与具体系统无关的任务调度核心、调度算法、CPU 掩码、惰性初始化和定时事件。', to: '/docs/architecture/overview' },
    { icon: 'server', title: '文件与进程组件', desc: 'axfs-ng-vfs、ax-fs-ng 与 rsext4 组成文件系统层，StarryOS 侧的 starry-kernel、starry-signal 与 starry-vm 承载进程、信号和地址空间语义。', to: '/docs/architecture/fs/overview' },
    { icon: 'chip', title: '虚拟化基础对象', desc: 'virtualization/ 提供 axvm、axaddrspace 与 axdevice 等基础对象，以及 arm_vgic、riscv_vplic、x86_vlapic 虚拟中断控制器，由 Axvisor 组合成完整的 VMM。', to: '/docs/architecture/axvisor' },
    { icon: 'plug', title: '设备能力接口', desc: 'dma-api、mmio-api、irq-framework 与 drivers/interface/ 下的 rdif-* 接口 crate 描述设备能力，使具体驱动实现不必依赖某一个系统的运行时。', to: '/docs/architecture/driver/overview' },
  ];

  return (
    <SectionShell
      id="capabilities"
      className="section-shell--capabilities"
      eyebrow="Core Capabilities"
      title="可组合的系统软件基础能力"
      description="工作区按领域划分出 components/、memory/、drivers/、fs/、net/、virtualization/ 与 platforms/ 七类目录，每一类只暴露明确的能力边界。三套系统按各自需求选择组合这些能力，组件本身不绑定任何一套系统的运行语义。"
    >
      <div className="capability-showcase" data-visual-pair>
        <CapabilityIllustration />

        <div className="capability-grid" data-visual-copy>
          {features.map((feature, index) => (
            <Link className={`capability-card ${staggerClass(index)}`} key={feature.title} to={feature.to}>
              <div className="feature-icon">{iconLibrary[feature.icon]}</div>
              <span className="capability-card__index">0{index + 1}</span>
              <div className="capability-card__body">
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
function ArchitectureIllustration() {
  const layers = [
    { code: 'ENTRY', label: '场景入口', detail: 'configuration', className: 'entry', x: 140, y: 32, width: 280 },
    { code: 'SYSTEM', label: '系统语义', detail: 'OS lifecycle & policy', className: 'system', x: 110, y: 152, width: 340 },
    { code: 'SHARED', label: '领域能力', detail: 'reusable no_std crates', className: 'shared', x: 75, y: 272, width: 410 },
    { code: 'PLATFORM', label: '平台边界', detail: 'arch · MMIO · DMA · IRQ', className: 'platform', x: 40, y: 392, width: 480 },
  ];

  return (
    <figure className="architecture-visual">
      <svg
        aria-labelledby="architecture-art-title architecture-art-description"
        className="architecture-art"
        role="img"
        viewBox="0 0 560 600"
      >
        <title id="architecture-art-title">TGOSKits four-layer architecture</title>
        <desc id="architecture-art-description">Four conceptual responsibility layers show scenario entry, system semantics, shared capabilities and platform contracts; this is not a complete Cargo dependency graph.</desc>
        <path className="architecture-art__axis" d="M280 112 V152 M280 232 V272 M280 352 V392" />
        <path className="architecture-art__arrow" d="M272 142 L280 150 L288 142 M272 262 L280 270 L288 262 M272 382 L280 390 L288 382" />
        {layers.map((layer, index) => (
          <g className={`architecture-art__layer architecture-art__layer--${layer.className}`} key={layer.code}>
            <rect height="80" rx="16" width={layer.width} x={layer.x} y={layer.y} />
            <circle cx={layer.x + 35} cy={layer.y + 40} r="18" />
            <text className="architecture-art__index" textAnchor="middle" x={layer.x + 35} y={layer.y + 46}>0{4 - index}</text>
            <text className="architecture-art__code" x={layer.x + 67} y={layer.y + 31}>{layer.code}</text>
            <text className="architecture-art__label" x={layer.x + 67} y={layer.y + 57}>{layer.label}</text>
            <text className="architecture-art__detail" textAnchor="end" x={layer.x + layer.width - 22} y={layer.y + 46}>{layer.detail}</text>
          </g>
        ))}
        <g className="architecture-art__base">
          <rect height="66" rx="16" width="520" x="20" y="512" />
          <text x="48" y="540">STABLE CONTRACTS</text>
          <text className="architecture-art__base-detail" x="48" y="562">workspace dependencies · traits · capability APIs</text>
        </g>
        <path className="architecture-art__axis" d="M280 472 V512" />
        <path className="architecture-art__arrow" d="M272 502 L280 510 L288 502" />
      </svg>
    </figure>
  );
}

function ArchitectureSection() {
  const architectureFlow = [
    { index: '04', code: 'ENTRY', label: '场景入口', desc: '定义目标系统的能力选择、构建参数与运行场景，同一批领域实现通过不同的 package 与 feature 组合装配成面向该场景的镜像。', items: ['feature / package selection', 'board / VM configuration'], tone: 'entry' },
    { index: '03', code: 'SYSTEM', label: '系统语义', desc: '实现内核生命周期、接口语义与运行策略：ArceOS 的模块化运行时、StarryOS 的 Linux 兼容语义和 Axvisor 的 VMM 都位于这一层。', items: ['OS lifecycle / syscall semantics', 'crate composition / policy'], tone: 'system' },
    { index: '02', code: 'SHARED', label: '领域能力', desc: '沉淀跨系统复用的内存、调度、I/O 与虚拟化机制，通过 no_std crate 与 trait 暴露能力，不引入具体系统的运行策略。', items: ['no_std reusable crates', 'traits / capability APIs'], tone: 'shared' },
    { index: '01', code: 'PLATFORM', label: '平台边界', desc: '适配 CPU 架构、固件、板级资源与设备访问，把启动、内存布局、时钟、中断和设备发现事实转换为上层可消费的稳定接口。', items: ['arch / board adapters', 'MMIO / DMA / IRQ contracts'], tone: 'platform' },
  ];

  const notes = [
    { title: '职责分层与实际依赖', desc: '四层用于解释代码应该放在哪里、允许依赖谁，不是严格的 Cargo 拓扑。例如 axvm 依赖 ArceOS 的运行时服务，部分领域组件之间也存在直接依赖；评估改动影响时，应核对直接依赖与 feature 条件，而不是只看目录层级。' },
    { title: '水平切分的复用边界', desc: '同一层的 crate 通过 trait 或能力接口解耦，系统以组合方式获取能力，而不是通过继承或全局单例；新的系统集成需求应落在接口层，而不是扩散到具体实现。' },
    { title: '副作用止于边界', desc: 'MMIO、DMA、IRQ、固件与调度能力只通过显式 API 跨层传递，避免组件内部隐式访问硬件。能力契约可以隔离大部分系统与平台差异，但具体耦合仍需按组件逐一审查。' },
  ];

  return (
    <SectionShell
      id="architecture"
      className="section-shell--architecture"
      eyebrow="Architecture"
      title="四层职责架构"
      description="四层描述的是职责归属而不是目录等级：场景入口选择能力与运行配置，系统语义定义接口行为和生命周期策略，领域能力沉淀可跨系统复用的实现，平台边界隔离 CPU、固件与板级差异。层与层之间只通过依赖声明、trait 和能力接口连接，完整依赖关系以组件关系图和构建配置为准。"
    >
      <div className="architecture-layout" data-visual-pair>
        <ArchitectureIllustration />
        <div className="architecture-explanations" data-visual-copy>
          {architectureFlow.map((layer) => (
            <article className={`architecture-explanation architecture-explanation--${layer.tone}`} key={layer.label}>
              <span className="architecture-explanation__index">{layer.index}</span>
              <div className="architecture-explanation__body">
                <div className="architecture-explanation__heading">
                  <span>{layer.code}</span><h3>{layer.label}</h3>
                </div>
                <p>{layer.desc}</p>
                <div className="architecture-explanation__items">
                  {layer.items.map((item) => (<code key={item}>{item}</code>))}
                </div>
              </div>
            </article>
          ))}
        </div>
      </div>
      <div className="architecture-notes">
        {notes.map((note, index) => (
          <article className="architecture-note" key={note.title}>
            <span className="architecture-note__index">0{index + 1}</span>
            <div>
              <h3>{note.title}</h3>
              <p>{note.desc}</p>
            </div>
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
      title="Git Subtree 组件同步工作流"
      description={`scripts/repo/repos.csv 登记 ${workspaceFacts.subtreeMappings} 条组件来源映射，每条记录包含上游地址、分支、目标目录与分类；其中 ${workspaceFacts.existingSubtreeTargets} 个目标目录当前存在，另有 ${workspaceFacts.subtreeMappings - workspaceFacts.existingSubtreeTargets} 条记录指向已移除的目录，同步前需要先清理。同步动作由维护者通过 repo.py 显式执行，组件改动不会自动写回上游仓库。`}
    >
      <ComponentWorkspaceDiagram />
    </SectionShell>
  );
}

/* ── Systems Section ─────────────────────────────────────── */
function SystemsSection() {
  const systems = [
    { id: 'arceos', accent: 'accent-arceos', name: 'ArceOS', subtitle: '组件化 Unikernel',
      desc: '应用、运行时与内核模块在编译期通过 Cargo feature 装配，只链接被选中的内存、任务、文件和网络能力；它同时是示例应用平台和其他两套系统的共享基础。',
      layers: [['Rust / C 应用'], ['ax-std', 'ax-libc'], ['API · runtime · 共享组件']], foundation: 'HAL · 平台与设备',
      items: ['编译期组件装配', '共享运行时与硬件抽象'] },
    { id: 'axvisor', accent: 'accent-axvisor', name: 'AxVisor', subtitle: 'Type-I Hypervisor',
      desc: '在 ArceOS 运行时之上组合虚拟机、客户机地址空间与虚拟设备，通过板级配置和 VM 配置描述 Guest 的资源与设备，并在 shell 或控制平面中管理其生命周期。',
      layers: [['Guest 01', 'Guest 02'], ['AxvmManager · axvm'], ['vCPU', '地址空间', '虚拟设备']], foundation: 'ArceOS · 宿主平台',
      items: ['客户机资源与生命周期管理', '多架构虚拟化组件'] },
    { id: 'starry', accent: 'accent-starry', name: 'StarryOS', subtitle: 'Linux 兼容操作系统',
      desc: '在 ArceOS 基础设施之上实现 Linux 兼容的进程、syscall、文件系统与 rootfs 语义，使未修改的 Linux 用户态程序可以直接运行在共享组件提供的底层机制之上。',
      layers: [['Linux 用户态 · Rootfs'], ['starry-kernel · syscall'], ['进程 / 信号', '内存 / 文件']], foundation: 'ArceOS · 共享组件与平台',
      items: ['Linux 用户态接口兼容', '进程与资源管理语义'] },
  ];

  return (
    <SectionShell
      id="systems"
      className="section-shell--systems"
      eyebrow="Systems"
      title="面向不同运行目标的三套系统"
      description="三套系统各自维护启动入口、配置集合、运行时语义和测试套件，同时复用同一批组件与 ArceOS 基础能力。每张卡片自下而上展示该系统的复用基础、实现主体和运行目标。"
    >
      <SystemsDiagram systems={systems} />
    </SectionShell>
  );
}

/* ── Docs Section ────────────────────────────────────────── */
function DocsSection() {
  const docs = [
    { title: '入门与运行', desc: '先了解项目边界和 workspace 模型，再按平台文档准备宿主环境，最后运行第一份系统镜像。', links: [{ label: '项目概览', to: '/docs/introduction/overview' }, { label: '快速开始', to: '/docs/quickstart/overview' }, { label: '架构与平台', to: '/docs/introduction/platform' }] },
    { title: '构建与验证', desc: '查询可用板卡名、写入构建配置、生成系统镜像，并通过命令参考和测试入口确认判定规则。', links: [{ label: '命令参考', to: '/docs/build/commands' }, { label: '配置系统', to: '/docs/build/configuration' }, { label: '测试入口', to: '/docs/build/test' }] },
    { title: '系统上手', desc: '分别查阅 ArceOS、StarryOS 与 Axvisor 的环境准备、rootfs 或 Guest 准备以及 QEMU 启动步骤。', links: [{ label: 'ArceOS', to: '/docs/quickstart/arceos' }, { label: 'StarryOS', to: '/docs/quickstart/starryos' }, { label: 'Axvisor', to: '/docs/quickstart/axvisor' }] },
    { title: '扩展与贡献', desc: '理解分层架构与目录边界，掌握 Git Subtree 组件同步机制以及代码和文档的贡献规范。', links: [{ label: '架构设计', to: '/docs/architecture/overview' }, { label: '仓库结构', to: '/docs/contributing/repo' }, { label: '文档贡献', to: '/docs/contributing/docs' }] },
  ];

  return (
    <SectionShell
      id="docs-map"
      className="section-shell--docs"
      eyebrow="Documentation Map"
      title="面向研发任务的文档导航"
      description="文档按研发任务组织：从环境准备和快速上手，到构建配置与测试判定，再到架构说明和仓库协作流程，每类任务的入口如下。"
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
function VerificationIllustration({ type }) {
  if (type === 'host') {
    return (
      <svg aria-hidden="true" className="verification-art" viewBox="0 0 360 190">
        <rect className="verification-art__surface" height="142" rx="16" width="304" x="28" y="24" />
        <path className="verification-art__line" d="M28 58 H332" />
        <circle className="verification-art__dot" cx="48" cy="41" r="5" />
        <circle className="verification-art__dot verification-art__dot--soft" cx="65" cy="41" r="5" />
        <circle className="verification-art__dot verification-art__dot--faint" cx="82" cy="41" r="5" />
        <path className="verification-art__prompt" d="M50 82 L60 90 L50 98" />
        <path className="verification-art__text" d="M74 90 H192" />
        <path className="verification-art__prompt" d="M50 112 L60 120 L50 128" />
        <path className="verification-art__text verification-art__text--short" d="M74 120 H160" />
        <circle className="verification-art__check-ring" cx="282" cy="91" r="17" />
        <path className="verification-art__check" d="M273 91 L279 97 L291 84" />
        <circle className="verification-art__check-ring" cx="282" cy="126" r="17" />
        <path className="verification-art__check" d="M273 126 L279 132 L291 119" />
      </svg>
    );
  }

  if (type === 'qemu') {
    return (
      <svg aria-hidden="true" className="verification-art" viewBox="0 0 360 190">
        <rect className="verification-art__surface" height="126" rx="16" width="288" x="36" y="22" />
        <path className="verification-art__line" d="M36 53 H324" />
        <path className="verification-art__line" d="M144 148 V164 M216 148 V164 M120 164 H240" />
        <rect className="verification-art__machine" height="62" rx="10" width="72" x="58" y="70" />
        <rect className="verification-art__machine" height="62" rx="10" width="72" x="144" y="70" />
        <rect className="verification-art__machine" height="62" rx="10" width="72" x="230" y="70" />
        <text className="verification-art__label" textAnchor="middle" x="94" y="107">A</text>
        <text className="verification-art__label" textAnchor="middle" x="180" y="107">S</text>
        <text className="verification-art__label" textAnchor="middle" x="266" y="107">X</text>
        <path className="verification-art__pulse" d="M70 42 H126 L134 34 L143 49 L151 39 L158 42 H290" />
      </svg>
    );
  }

  return (
    <svg aria-hidden="true" className="verification-art" viewBox="0 0 360 190">
      <rect className="verification-art__board" height="136" rx="20" width="246" x="57" y="25" />
      <rect className="verification-art__chip" height="68" rx="10" width="82" x="139" y="59" />
      <path className="verification-art__line" d="M103 46 V72 H139 M103 140 V114 H139 M257 46 V72 H221 M257 140 V114 H221" />
      <path className="verification-art__pins" d="M151 52 V59 M166 52 V59 M180 52 V59 M194 52 V59 M209 52 V59 M151 127 V134 M166 127 V134 M180 127 V134 M194 127 V134 M209 127 V134" />
      <circle className="verification-art__status" cx="86" cy="51" r="7" />
      <circle className="verification-art__status verification-art__status--soft" cx="86" cy="75" r="7" />
      <path className="verification-art__pulse" d="M80 108 H99 L108 94 L119 122 L129 108 H139" />
      <rect className="verification-art__port" height="30" rx="5" width="32" x="271" y="89" />
    </svg>
  );
}

function QualitySection() {
  const lanes = [
    { type: 'host', status: 'Local', scope: 'Crate', signal: '快速反馈', title: 'Host 侧组件验证', desc: '按 std_crates.csv 白名单在宿主机上运行标准库测试，并对改动包执行静态检查，不需要目标系统或模拟器即可发现组件级问题。', items: ['cargo xtask clippy', 'cargo xtask test', '按项目清单展开功能与目标组合'] },
    { type: 'qemu', status: 'System', scope: 'System image', signal: '完整语义', title: 'QEMU 系统级验证', desc: '构建目标系统镜像并在 QEMU 中运行，由 test-suit 配置中的成功与失败规则判定 syscall、进程、设备和 Guest 引导行为是否符合预期。', items: ['ArceOS Rust / C / axtest 用例', 'StarryOS grouped system + TTY 输入', 'Axvisor Guest 引导与交互'] },
    { type: 'board', status: 'Scenario', scope: 'Physical board', signal: '真实设备', title: '板级场景回归', desc: '在自托管板卡上执行端到端场景，确认启动、设备与 Guest 行为在真实硬件上与 QEMU 的结论一致；执行与否取决于硬件可用性。', items: ['platforms/* 编译与启动验证', 'VM / Guest 配置兼容性回归', '共享 crate 的多系统影响面检查'] },
  ];

  return (
    <SectionShell
      id="quality"
      className="section-shell--quality"
      eyebrow="Verification"
      title="从组件检查到真实板卡的三级验证"
      description="验证按成本和覆盖面从低到高分为三级：先在宿主机上以最短反馈路径发现组件问题，再用 QEMU 运行完整系统镜像检查集成与运行语义，最后在自托管板卡上确认平台适配和真实设备行为。CI 按改动的影响范围选择其中若干级执行。"
    >
      <div className="quality-gallery" aria-label="Three verification layers from host to physical board">
        {lanes.map((lane, i) => (
          <article className={`quality-card quality-card--${lane.type} ${staggerClass(i)}`} key={lane.title}>
            <div className="quality-card__visual">
              <div className="quality-card__caption"><span>0{i + 1}</span><strong>{lane.signal}</strong></div>
              <VerificationIllustration type={lane.type} />
            </div>
            <div className="quality-card__body">
              <div className="quality-card__meta"><span>{lane.status}</span><code>{lane.scope}</code></div>
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

/* ── Hardware Enablement Section ─────────────────────────── */
function HardwareSection() {
  const architectures = [
    { arch: 'aarch64', target: 'aarch64-unknown-none-softfloat', platform: 'QEMU virt；板卡验证以 OrangePi-5-Plus 为主', note: 'ArceOS · StarryOS · Axvisor' },
    { arch: 'riscv64', target: 'riscv64gc-unknown-none-elf', platform: 'QEMU virt；Axvisor 启用 sstc', note: 'ArceOS · StarryOS · Axvisor' },
    { arch: 'x86_64', target: 'x86_64-unknown-none', platform: 'q35 · ACPI；Axvisor 的 QEMU 用例需要 KVM 与 Intel VMX / AMD SVM', note: 'ArceOS · StarryOS · Axvisor' },
    { arch: 'loongarch64', target: 'loongarch64-unknown-none-softfloat', platform: 'QEMU virt；Axvisor 使用动态 UEFI/OVMF，需要 LVZ 容器', note: 'ArceOS · StarryOS · Axvisor' },
  ];

  const driverCategories = [
    { icon: 'server', title: '块设备', items: ['sdhci-host', 'dwmmc-host', 'phytium-mci-host', 'nvme-driver'] },
    { icon: 'pulse', title: '网络', items: ['realtek-rtl8125', 'eth-intel', 'fxmac_rs', 'rd-net'] },
    { icon: 'orbit', title: '中断控制器', items: ['arm-gic-driver', 'ax-riscv-plic', 'rdif-intc'] },
    { icon: 'layers', title: 'PCIe', items: ['pcie', 'rk3588-pci', 'rdif-pcie'] },
    { icon: 'plug', title: 'USB', items: ['crab-usb', 'usb-if', 'usb-serial'] },
    { icon: 'chip', title: 'AI 与多媒体', items: ['rockchip-npu', 'k230-kpu', 'sg2002-tpu', 'rockchip-rga', 'rockchip-jpeg'] },
    { icon: 'grid', title: '平台设备', items: ['rockchip-pwm', 'ax-arm-pl031', 'some-serial', 'arm-scmi-rs'] },
  ];

  const boardEvidence = [
    { board: 'OrangePi-5-Plus', systems: 'ArceOS PMU · StarryOS suites · Axvisor Linux/Starry Guest' },
    { board: 'Phytium Pi', systems: 'Axvisor Linux Guest' },
    { board: 'ROC-RK3568-PC', systems: 'Axvisor Linux Guest' },
    { board: 'ASUS NUC15 CRH', systems: 'Axvisor Linux Guest' },
    { board: 'AKA-00-SG2002', systems: 'StarryOS suites' },
    { board: 'VisionFive 2', systems: 'StarryOS suites' },
    { board: 'JL LSGD2K10', systems: 'StarryOS suites' },
  ];

  return (
    <SectionShell
      id="hardware"
      className="section-shell--hardware"
      eyebrow="Hardware Enablement"
      title="四架构平台与设备使能"
      description="四种架构都具备 ArceOS、StarryOS 与 Axvisor 的 QEMU 构建与测试入口，差异集中在虚拟平台模型和启动路径上。drivers/ 按设备类型组织驱动核心与具体实现，通过 rdif-* 等能力接口接入系统。物理板卡用例登记在 CI 清单中，实际执行取决于变更路由、自托管运行器和硬件可用性。"
    >
      <div className="hardware-layout">
        <div className="hardware-platforms">
          <div className="hardware-panel__heading">
            <span>01</span>
            <div><h3>四架构 QEMU 配置</h3><p>三套系统均有对应构建与测试入口</p></div>
          </div>
          <div className="hardware-platform-table">
            {architectures.map((item) => (
              <article className="hardware-platform-row" key={item.arch}>
                <strong>{item.arch}</strong>
                <div><code>{item.target}</code><span>{item.platform}</span></div>
                <small>{item.note}</small>
              </article>
            ))}
          </div>
        </div>

        <div className="hardware-drivers">
          <div className="hardware-panel__heading">
            <span>02</span>
            <div><h3>drivers/ 设备类别</h3><p>设备核心实现通过 RDIF 等能力接口接入系统</p></div>
          </div>
          <div className="hardware-driver-catalog">
            {driverCategories.map((category) => (
              <article className="hardware-driver-row" key={category.title}>
                <div className="feature-icon">{iconLibrary[category.icon]}</div>
                <div><h4>{category.title}</h4><p>{category.items.join(' · ')}</p></div>
              </article>
            ))}
          </div>
        </div>
      </div>

      <section className="hardware-board-evidence" aria-labelledby="hardware-board-title">
        <div className="hardware-board-evidence__header">
          <div className="hardware-board-evidence__heading">
            <p className={layout.eyebrow}>Self-hosted CI</p>
            <h3 id="hardware-board-title">实体板卡验证</h3>
            <p>当前 CI 清单登记的板卡与验证场景</p>
          </div>
          <Link className={layout.secondaryButton} to="/docs/introduction/platform">平台支持范围</Link>
        </div>
        <ul className="hardware-board-list">
          {boardEvidence.map((item) => (
            <li className="hardware-board" key={item.board}>
              <div className="hardware-board__title"><span className="hardware-board__icon" aria-hidden="true">{iconLibrary.chip}</span><h4>{item.board}</h4></div>
              <ul className="hardware-board__scenarios">{item.systems.split(' · ').map(scenario => <li key={scenario}>{scenario}</li>)}</ul>
            </li>
          ))}
        </ul>
      </section>
    </SectionShell>
  );
}

/* ── Home Page ───────────────────────────────────────────── */
export default function Home() {
  const pageRef = useVisualHeight();
  const { siteConfig } = useDocusaurusContext();
  useScrollReveal();

  return (
    <Layout title={siteConfig.title} description={siteConfig.tagline} wrapperClassName="home site-showcase">
      <main ref={pageRef}>
      <HeroBanner />
      <CapabilitySection />
      <SystemsSection />
      <ArchitectureSection />
      <ComponentWorkspaceSection />
      <HardwareSection />
      <QualitySection />
      <DocsSection />
      </main>
    </Layout>
  );
}
