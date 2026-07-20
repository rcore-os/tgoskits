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
    { name: 'axallocator', path: 'memory/axallocator', tone: 'memory' },
    { name: 'arm_vcpu', path: 'virtualization/arm_vcpu', tone: 'virtualization' },
    { name: 'rockchip-npu', path: 'drivers/npu/rockchip-npu', tone: 'driver' },
  ];

  const hubItems = [
    { title: '外部仓库汇聚', desc: ['50 个 subtree 映射', 'OS · 内存 · 驱动 · VFS · 虚拟化'] },
    { title: 'Subtree 同步工具', desc: ['repo.py list / pull / push', '集成验证后同步回上游'] },
    { title: '来源边界清晰', desc: ['repos.csv · target_dir · category'] },
  ];

  return (
    <div className="workspace-diagram" aria-label="Git Subtree component workspace workflow">
      <div className="workspace-diagram__title">50 个 Git Subtree 映射：外部仓库 ↔ 统一工作区 ↔ 上游</div>
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
    { label: '工作区包', value: '184' },
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
            ArceOS、StarryOS、Axvisor 三套系统共享由 184 个 package 组成的 Cargo workspace，
            通过 cargo xtask 统一执行构建、镜像生成、QEMU 运行与分层验证，形成从组件开发到系统集成的可复现工程流程。
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
function CapabilityIllustration() {
  const domains = [
    { name: 'components/', detail: 'scheduler · fs · process', y: 140 },
    { name: 'memory/', detail: 'allocator · page table · DMA/MMIO', y: 245 },
    { name: 'drivers/', detail: 'blk · net · USB · NPU · PCI', y: 350 },
    { name: 'virtualization/', detail: 'vCPU · VM · address space', y: 455 },
  ];

  const systems = [
    { className: 'arceos', name: 'ArceOS', detail: 'Modular OS', y: 140 },
    { className: 'starry', name: 'StarryOS', detail: 'Linux-compatible OS', y: 275 },
    { className: 'axvisor', name: 'Axvisor', detail: 'Type-I hypervisor', y: 410 },
  ];

  const architectures = ['aarch64', 'riscv64', 'x86_64', 'loongarch64'];

  return (
    <figure className="capability-illustration card-reveal stagger-1">
      <svg
        aria-labelledby="capability-art-title capability-art-description"
        className="capability-art"
        role="img"
        viewBox="0 0 1200 620"
      >
        <title id="capability-art-title">TGOSKits workspace capability map</title>
        <desc id="capability-art-description">
          Cargo xtask orchestrates reusable component, memory, driver and virtualization crates for ArceOS, StarryOS and Axvisor across four CPU architectures.
        </desc>

        <rect className="capability-art__frame" height="618" rx="28" width="1198" x="1" y="1" />

        <g className="capability-art__connections">
          <path d="M600 100 V165" />
          {domains.map((domain) => (
            <path d={`M290 ${domain.y + 37.5} H350 Q390 ${domain.y + 37.5} 410 285`} key={domain.name} />
          ))}
          {systems.map((system) => (
            <path d={`M770 315 Q820 315 900 ${system.y + 45}`} key={system.name} />
          ))}
          <path d="M590 465 V505 H865" />
          <path d="M415 505 H590" />
          {[405, 555, 705, 855].map((x) => (<path d={`M${x} 505 V535`} key={x} />))}
        </g>

        <g className="capability-art__xtask">
          <rect height="70" rx="18" width="280" x="460" y="30" />
          <text className="capability-art__overline" textAnchor="middle" x="600" y="56">UNIFIED ORCHESTRATION</text>
          <text className="capability-art__title" textAnchor="middle" x="600" y="83">cargo xtask</text>
        </g>

        {domains.map((domain) => (
          <g className="capability-art__domain" key={domain.name}>
            <rect height="75" rx="16" width="250" x="40" y={domain.y} />
            <path d={`M68 ${domain.y + 23} h18 l7 8 h31 v25 h-56 z`} />
            <text className="capability-art__node-title" x="138" y={domain.y + 32}>{domain.name}</text>
            <text className="capability-art__node-copy" x="138" y={domain.y + 55}>{domain.detail}</text>
          </g>
        ))}

        <g className="capability-art__workspace">
          <rect height="300" rx="28" width="360" x="410" y="165" />
          <text className="capability-art__overline" textAnchor="middle" x="590" y="207">TGOSKITS CARGO WORKSPACE</text>
          <text className="capability-art__metric" textAnchor="middle" x="590" y="292">184</text>
          <text className="capability-art__metric-label" textAnchor="middle" x="590" y="326">workspace packages</text>
          <line x1="465" x2="715" y1="350" y2="350" />
          <text className="capability-art__workspace-copy" textAnchor="middle" x="590" y="385">build · run · test · image · board</text>
          <text className="capability-art__workspace-copy" textAnchor="middle" x="590" y="418">shared crates, explicit OS boundaries</text>
        </g>

        {systems.map((system) => (
          <g className={`capability-art__system capability-art__system--${system.className}`} key={system.name}>
            <rect height="90" rx="18" width="250" x="900" y={system.y} />
            <text className="capability-art__system-title" x="930" y={system.y + 39}>{system.name}</text>
            <text className="capability-art__node-copy" x="930" y={system.y + 65}>{system.detail}</text>
          </g>
        ))}

        <text className="capability-art__overline" textAnchor="middle" x="630" y="526">ARCHITECTURE TARGETS</text>
        {architectures.map((architecture, index) => (
          <g className="capability-art__arch" key={architecture}>
            <rect height="48" rx="14" width="130" x={340 + index * 150} y="535" />
            <text textAnchor="middle" x={405 + index * 150} y="566">{architecture}</text>
          </g>
        ))}
      </svg>
      <figcaption>
        <code>components/</code>、<code>memory/</code>、<code>drivers/</code> 与 <code>virtualization/</code> 通过统一 workspace 为三套系统提供基础能力。
      </figcaption>
    </figure>
  );
}

function CapabilitySection() {
  const features = [
    { icon: 'orbit', title: '统一工程编排', desc: 'cargo xtask 提供 ArceOS、StarryOS、Axvisor、镜像、板卡与测试命令的统一入口。', to: '/docs/build/overview' },
    { icon: 'grid', title: '内存基础能力', desc: 'allocator、地址类型、memory set 与多架构页表实现集中在 memory/，供系统按需组合。', to: '/docs/development/components' },
    { icon: 'layers', title: '调度与同步原语', desc: 'axsched、cpumask、kspin、kernel_guard 与 lockdep 提供可复用的内核运行时基础。', to: '/docs/development/components' },
    { icon: 'server', title: '文件与进程组件', desc: 'axfs-ng-vfs、rsext4、starry-process、starry-signal 与 starry-vm 承载明确的领域语义。', to: '/docs/development/components' },
    { icon: 'chip', title: '虚拟化基础对象', desc: 'virtualization/ 提供 VM、vCPU、地址空间、虚拟设备及各架构中断控制器实现。', to: '/docs/development/components' },
    { icon: 'plug', title: '设备能力接口', desc: 'dma-api、mmio-api、irq-framework 与 RDIF 接口 crate 将资源访问从具体 OS glue 中分离。', to: '/docs/architecture/overview' },
  ];

  return (
    <SectionShell
      id="capabilities"
      className="section-shell--capabilities"
      eyebrow="Core Capabilities"
      title="可组合的系统软件基础能力"
      description="统一的 Cargo workspace 汇聚工程编排、内存管理、调度同步、文件与进程、虚拟化及设备接口，为三套系统提供可复用的实现基础。"
    >
      <div className="capability-showcase">
        <CapabilityIllustration />
        <div className="capability-narrative card-reveal stagger-2">
          <span className="capability-narrative__eyebrow">Repository-backed view</span>
          <h3>领域能力统一复用，运行语义保持隔离</h3>
          <p>
            可复用机制以 crate 纳入统一 workspace，OS glue、平台适配与运行策略由各系统独立实现；
            cargo xtask 在不破坏边界的前提下统一编排构建、镜像、运行和验证流程。
          </p>
          <dl className="capability-facts">
            <div><dt>184</dt><dd>workspace packages</dd></div>
            <div><dt>3</dt><dd>system paths</dd></div>
            <div><dt>4</dt><dd>CPU architectures</dd></div>
          </dl>
          <Link className="capability-narrative__link" to="/docs/development/components">查看组件与边界</Link>
        </div>
      </div>

      <div className="capability-grid">
        {features.map((feature, index) => (
          <Link className={`capability-card ${staggerClass(index)}`} key={feature.title} to={feature.to}>
            <div className="feature-icon">{iconLibrary[feature.icon]}</div>
            <span className="capability-card__index">0{index + 1}</span>
            <div className="capability-card__body">
              <h3>{feature.title}</h3>
              <p>{feature.desc}</p>
            </div>
            <span aria-hidden="true" className="capability-card__arrow">→</span>
          </Link>
        ))}
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
        <desc id="architecture-art-description">Four increasingly broad layers show that scenario entry depends on system semantics, reusable domain capabilities and platform contracts.</desc>
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
      <figcaption>依赖方向始终向下：上层选择能力，下层提供稳定契约。</figcaption>
    </figure>
  );
}

function ArchitectureSection() {
  const architectureFlow = [
    { index: '04', code: 'ENTRY', label: '场景入口', desc: '定义目标系统的能力选择、构建参数与运行场景', items: ['feature / package selection', 'board / VM configuration'], tone: 'entry' },
    { index: '03', code: 'SYSTEM', label: '系统语义', desc: '实现内核生命周期、接口语义与运行策略', items: ['OS lifecycle / syscall semantics', 'crate composition / policy'], tone: 'system' },
    { index: '02', code: 'SHARED', label: '领域能力', desc: '沉淀跨系统复用的内存、调度、I/O 与虚拟化机制', items: ['no_std reusable crates', 'traits / capability APIs'], tone: 'shared' },
    { index: '01', code: 'PLATFORM', label: '平台边界', desc: '适配 CPU 架构、固件、板级资源与设备访问', items: ['arch / board adapters', 'MMIO / DMA / IRQ contracts'], tone: 'platform' },
  ];

  const notes = [
    { title: '自底向上的依赖约束', desc: '下层 crate 不依赖上层实现，组件层不引用系统层代码，平台层不感知具体系统。依赖方向单一，修改影响可控。' },
    { title: '水平切分的复用边界', desc: '同一层的 crate 通过 trait 或接口抽象解耦，系统通过组合而非继承获取能力，新增系统实现无需修改现有组件。' },
    { title: '副作用止于边界', desc: 'MMIO、DMA、IRQ、固件与调度能力只通过显式 API 跨层传递；共享逻辑依赖能力契约，不直接耦合具体 OS 或平台实现。' },
  ];

  return (
    <SectionShell
      id="architecture"
      className="section-shell--architecture"
      eyebrow="Architecture"
      title="四层单向依赖架构"
      description="场景入口、系统语义、领域能力与平台边界构成自上而下的依赖链，跨层交互通过 workspace 依赖、trait 和 capability API 建立稳定契约。"
    >
      <div className="architecture-layout">
        <ArchitectureIllustration />
        <div className="architecture-explanations">
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
      title="50 个 Git Subtree 映射的同步工作流"
      description="scripts/repo/repos.csv 维护外部仓库与工作区目录的 50 组映射，repo.py 执行双向同步，使独立仓库演进、集成验证与上游回推保持一致。"
    >
      <ComponentWorkspaceDiagram />
    </SectionShell>
  );
}

/* ── Systems Section ─────────────────────────────────────── */
function SystemsSection() {
  const systems = [
    { accent: 'accent-arceos', name: 'ArceOS', subtitle: '模块化内核', tag: '组合系统', desc: '通过配置组合 axalloc、axtask、axfs、axnet、axhal 等模块，生成面向具体应用场景的系统镜像。', items: ['四架构 Rust、C 与 axtest 用例', '示例覆盖基础运行与设备场景', '基于 feature 和配置裁剪模块能力'] },
    { accent: 'accent-starry', name: 'StarryOS', subtitle: 'Linux 兼容 OS', tag: '用户态兼容', desc: '实现 Linux 系统调用、ELF 加载、进程与信号语义，并通过 rootfs 和用户态程序验证兼容性。', items: ['四架构系统调用分组测试', '四架构 TTY 输入测试', '板测覆盖网络、USB、PCIe 与 NPU'] },
    { accent: 'accent-axvisor', name: 'Axvisor', subtitle: 'Type-I Hypervisor', tag: '虚拟化运行时', desc: '管理 VM、vCPU、虚拟地址空间与虚拟设备，并通过静态或动态平台配置启动不同 Guest。', items: ['四架构 Guest 启动冒烟测试', 'x86_64 支持 VMX 与 SVM', 'LoongArch64 支持动态 UEFI 启动'] },
  ];

  return (
    <SectionShell
      id="systems"
      className="section-shell--systems"
      eyebrow="Systems"
      title="面向不同运行目标的三套系统"
      description="ArceOS 提供模块化内核组合，StarryOS 实现 Linux 用户态兼容，Axvisor 提供 Type-I 虚拟化；三者复用工作区基础能力并独立维护运行语义。"
    >
      <SystemsDiagram systems={systems} />
    </SectionShell>
  );
}

/* ── Docs Section ────────────────────────────────────────── */
function DocsSection() {
  const docs = [
    { title: '入门与运行', desc: '了解项目定位、开发环境与三套系统的 QEMU 启动流程。', links: [{ label: '项目概览', to: '/docs/introduction/overview' }, { label: '快速开始', to: '/docs/quickstart/overview' }] },
    { title: '构建与验证', desc: '配置目标架构和平台，生成系统镜像并执行相应测试。', links: [{ label: '命令参考', to: '/docs/build/commands' }, { label: '配置系统', to: '/docs/build/configuration' }, { label: '测试入口', to: '/docs/build/test' }] },
    { title: '系统开发', desc: '查阅 ArceOS、StarryOS 与 Axvisor 的目录结构、开发接口和运行方式。', links: [{ label: 'ArceOS', to: '/docs/development/arceos' }, { label: 'StarryOS', to: '/docs/development/starryos' }, { label: 'Axvisor', to: '/docs/development/axvisor' }] },
    { title: '扩展与贡献', desc: '掌握组件边界、仓库同步机制以及代码与文档贡献规范。', links: [{ label: '组件开发', to: '/docs/development/components' }, { label: '仓库结构', to: '/docs/contributing/repo' }, { label: '文档贡献', to: '/docs/contributing/docs' }] },
  ];

  return (
    <SectionShell
      id="docs-map"
      className="section-shell--docs"
      eyebrow="Documentation Map"
      title="面向研发任务的文档导航"
      description="文档体系覆盖环境准备、系统构建、运行验证、组件开发与贡献流程，可按当前研发任务进入对应指南和命令参考。"
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
    { type: 'host', status: 'Local', scope: 'Crate', signal: '快速反馈', title: 'Host 侧组件验证', desc: '在宿主机上执行标准库测试与静态检查，不启动目标系统即可发现组件级问题。', items: ['cargo test -p <crate>', 'cargo xtask clippy', 'cargo xtask test'] },
    { type: 'qemu', status: 'System', scope: 'System image', signal: '完整语义', title: 'QEMU 系统级验证', desc: '构建目标系统镜像并在 QEMU 中运行，检查 syscall、进程、设备与 Guest 引导行为。', items: ['ArceOS example 运行检查', 'StarryOS rootfs + shell 启动', 'Axvisor Guest 引导与交互'] },
    { type: 'board', status: 'Scenario', scope: 'Physical board', signal: '真实设备', title: '板级场景回归', desc: '在 self-hosted 板卡上执行端到端场景，确认平台适配与真实硬件行为。', items: ['platforms/* 编译与启动验证', 'VM / Guest 配置兼容性回归', '共享 crate 的多系统影响面检查'] },
  ];

  return (
    <SectionShell
      id="quality"
      className="section-shell--quality"
      eyebrow="Verification"
      title="从组件检查到真实板卡的三级验证"
      description="Host 测试与静态检查覆盖 crate 级正确性，QEMU 验证系统集成与运行语义，self-hosted 板卡回归确认平台适配和设备行为。"
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
    { arch: 'aarch64', target: 'aarch64-unknown-none-softfloat', platform: 'QEMU virt', note: 'ArceOS · StarryOS · Axvisor' },
    { arch: 'riscv64', target: 'riscv64gc-unknown-none-elf', platform: 'QEMU virt + SSTC', note: 'ArceOS · StarryOS · Axvisor' },
    { arch: 'x86_64', target: 'x86_64-unknown-none', platform: 'Q35 · ACPI · VMX/SVM', note: 'ArceOS · StarryOS · Axvisor' },
    { arch: 'loongarch64', target: 'loongarch64-unknown-none-softfloat', platform: 'QEMU virt · dynamic UEFI', note: 'ArceOS · StarryOS · Axvisor' },
  ];

  const driverCategories = [
    { icon: 'server', title: '块设备', items: ['sdhci-host', 'dwmmc-host', 'nvme-driver'] },
    { icon: 'pulse', title: '网络', items: ['realtek-rtl8125', 'eth-intel', 'fxmac_rs'] },
    { icon: 'orbit', title: '中断控制器', items: ['arm-gic-driver', 'riscv_plic', 'rdif-intc'] },
    { icon: 'layers', title: 'PCIe', items: ['pcie', 'rk3588-pci', 'rdif-pcie'] },
    { icon: 'plug', title: 'USB', items: ['usb-host', 'usb-if', 'usb-serial'] },
    { icon: 'chip', title: 'AI 与多媒体', items: ['rockchip-npu', 'k230-kpu', 'sg2002-tpu'] },
    { icon: 'grid', title: '平台设备', items: ['rockchip-pwm', 'arm_pl031', 'arm-scmi-rs'] },
  ];

  const boardEvidence = [
    { board: 'OrangePi-5-Plus', systems: 'StarryOS · Axvisor' },
    { board: 'Phytium Pi', systems: 'Axvisor' },
    { board: 'ROC-RK3568-PC', systems: 'Axvisor' },
    { board: 'ASUS NUC15 CRH', systems: 'Axvisor' },
    { board: 'AKA-00-SG2002', systems: 'StarryOS' },
    { board: 'VisionFive 2', systems: 'StarryOS' },
  ];

  return (
    <SectionShell
      id="hardware"
      className="section-shell--hardware"
      eyebrow="Hardware Enablement"
      title="四架构平台与设备使能"
      description="aarch64、riscv64、x86_64 与 loongarch64 均具备三套系统的 QEMU 配置；drivers/ 提供分类型设备实现，self-hosted CI 持续验证关键实体板卡。"
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

      <div className="hardware-board-evidence">
        <div className="hardware-board-evidence__heading">
          <span>Self-hosted CI</span>
          <strong>当前持续执行的板卡用例</strong>
        </div>
        <div className="hardware-board-list">
          {boardEvidence.map((item) => (
            <div key={item.board}><strong>{item.board}</strong><span>{item.systems}</span></div>
          ))}
        </div>
        <Link className="hardware-board-evidence__link" to="/docs/introduction/platform">查看完整支持与证据矩阵</Link>
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
      <SystemsSection />
      <ArchitectureSection />
      <ComponentWorkspaceSection />
      <HardwareSection />
      <QualitySection />
      <DocsSection />
    </Layout>
  );
}
