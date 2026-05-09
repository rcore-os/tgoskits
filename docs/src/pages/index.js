import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import './index.css';

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

function SectionShell({ id, className, eyebrow, title, description, children, framed = true }) {
  return (
    <section className={`section-shell ${className || ''}`} id={id}>
      <div className="section-shell__inner">
        <div className={`section-shell__surface${framed ? '' : ' section-shell__surface--open'}`}>
          <div className="section-header">
            <p className="eyebrow">{eyebrow}</p>
            <h2>{title}</h2>
            <p>{description}</p>
          </div>
          {children}
        </div>
      </div>
    </section>
  );
}

function HeroBanner() {
  const heroStats = [
    { label: '核心系统', value: '3' },
    { label: '共享组件', value: '140+' },
    { label: '主流架构', value: '4' },
    { label: '统一命令入口', value: 'xtask' },
  ];

  const quickLinks = [
    { label: '项目概览', to: '/docs/introduction/overview' },
    { label: '快速开始', to: '/docs/quickstart/overview' },
    { label: '构建系统', to: '/docs/design/reference/build-system' },
    { label: '组件视图', to: '/docs/design/reference/components' },
  ];

  return (
    <section className="hero-banner" id="hero" aria-label="TGOSKits overview banner">
      <svg className="hero-background-svg" viewBox="0 0 1200 800" preserveAspectRatio="xMidYMid slice" aria-hidden="true">
        <defs>
          <linearGradient id="heroGrad1" x1="0%" y1="0%" x2="100%" y2="100%">
            <stop offset="0%" stopColor="var(--hero-grad-start-1)" />
            <stop offset="100%" stopColor="var(--hero-grad-end-1)" />
          </linearGradient>
          <linearGradient id="heroGrad2" x1="100%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="var(--hero-grad-start-2)" />
            <stop offset="100%" stopColor="var(--hero-grad-end-2)" />
          </linearGradient>
        </defs>
        <rect width="1200" height="800" fill="url(#heroGrad1)" opacity="0.28" />
        <path d="M0,100 Q300,50 600,100 T1200,100" stroke="url(#heroGrad2)" strokeWidth="2" fill="none" opacity="0.4" className="hero-wave-top" />
        <path d="M0,120 Q300,80 600,120 T1200,120" stroke="url(#heroGrad2)" strokeWidth="1" fill="none" opacity="0.2" className="hero-wave-top" />
        <circle cx="150" cy="250" r="80" fill="none" stroke="url(#heroGrad2)" strokeWidth="2" opacity="0.2" className="hero-circle-anim" />
        <circle cx="150" cy="250" r="60" fill="none" stroke="url(#heroGrad2)" strokeWidth="1" opacity="0.1" className="hero-circle-anim-delayed" />
        <circle cx="1100" cy="600" r="100" fill="none" stroke="url(#heroGrad2)" strokeWidth="2" opacity="0.15" className="hero-circle-anim-reverse" />
        <line x1="100" y1="650" x2="300" y2="700" stroke="url(#heroGrad2)" strokeWidth="1" opacity="0.3" className="hero-line-anim" />
        <line x1="950" y1="150" x2="1100" y2="200" stroke="url(#heroGrad2)" strokeWidth="1" opacity="0.3" className="hero-line-anim-reverse" />
        <circle cx="600" cy="150" r="4" fill="url(#heroGrad2)" opacity="0.6" className="hero-dot-pulse" />
        <circle cx="200" cy="600" r="3" fill="url(#heroGrad2)" opacity="0.5" className="hero-dot-pulse" />
        <circle cx="1000" cy="400" r="3" fill="url(#heroGrad2)" opacity="0.5" className="hero-dot-pulse-delayed" />
      </svg>

      <div className="hero-content">
        <div className="hero-copy">
          <p className="eyebrow">Operating Systems and Virtualization Workspace</p>
          <h1>
            <span>TGOSKits</span>
            <em>面向系统软件研发的一体化工作区</em>
          </h1>
          <p className="lead">
            汇聚 ArceOS、StarryOS、Axvisor 与共享组件栈，在同一仓库中组织系统内核、
            虚拟化、平台适配、测试验证和构建自动化，形成连贯的工程开发入口。
          </p>
          <div className="hero-actions">
            <Link className="button button--primary button--hero" to="/docs/introduction/overview">
              阅读概览
            </Link>
            <Link className="button button--outline button--hero" to="/docs/quickstart/overview">
              开始上手
            </Link>
            <Link className="button button--secondary button--hero" to="https://github.com/rcore-os/tgoskits">
              GitHub
            </Link>
          </div>
          <div className="hero-quicklinks">
            {quickLinks.map((link) => (
              <Link key={link.label} className="hero-quicklink" to={link.to}>
                {link.label}
              </Link>
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
        <defs>
          <linearGradient id="waveFill" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="var(--hero-wave-color)" />
            <stop offset="100%" stopColor="var(--home-base)" />
          </linearGradient>
        </defs>
        <path d="M0,20 Q300,0 600,20 T1200,20 L1200,100 L0,100 Z" fill="url(#waveFill)" />
        <path d="M0,30 Q300,10 600,30 T1200,30 L1200,100 L0,100 Z" fill="var(--home-base)" opacity="0.68" />
      </svg>
    </section>
  );
}

function HeroTerminal() {
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
      <pre className="hero-terminal-screen">{`$ cargo xtask arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf
[ArceOS] Hello, world!

$ cargo xtask starry rootfs --arch riscv64
$ cargo xtask starry qemu --arch riscv64
[StarryOS] shell started.

$ cargo xtask axvisor qemu --arch aarch64
[Axvisor] Guest[0] ArceOS running.`}</pre>
      <div className="hero-terminal-footer">
        <span>ArceOS</span>
        <span>StarryOS</span>
        <span>Axvisor</span>
        <span>Shared Crates</span>
      </div>
    </div>
  );
}

function CapabilitySection() {
  const features = [
    {
      icon: 'orbit',
      title: '统一入口',
      desc: '围绕根目录文档与 cargo xtask 组织日常开发入口，降低系统间切换成本。',
      to: '/docs/design/reference/build-system',
    },
    {
      icon: 'layers',
      title: '组件共享',
      desc: '基础能力以独立 crate 组织，被多个系统路径复用，职责边界更清晰。',
      to: '/docs/design/reference/components',
    },
    {
      icon: 'shield',
      title: '安全实现',
      desc: '以内存安全为优先，围绕 Rust 构建可维护的系统软件组件与接口。',
      to: '/docs/design/architecture/arch',
    },
    {
      icon: 'pulse',
      title: '多架构支持',
      desc: '围绕 riscv64、aarch64、x86_64、loongarch64 形成可迁移的构建与验证链路。',
      to: '/docs/introduction/hardware',
    },
    {
      icon: 'chip',
      title: '构建闭环',
      desc: '从配置、构建、镜像生成到 QEMU 运行与快照管理形成完整流程。',
      to: '/docs/design/build/flow',
    },
    {
      icon: 'server',
      title: '验证体系',
      desc: '从 host 侧测试到系统级运行验证，覆盖组件、系统和平台多个层面。',
      to: '/docs/design/test/overview',
    },
  ];

  return (
    <SectionShell
      id="capabilities"
      className="section-shell--capabilities"
      eyebrow="Core Capabilities"
      title="围绕系统软件工程构建统一能力面"
      description="统一入口、组件共享、安全实现、多架构适配、构建闭环与分层验证构成项目的六项核心能力。"
      framed={false}
    >
      <div className="feature-grid">
        {features.map((feature) => (
          <Link className="feature-card" key={feature.title} to={feature.to}>
            <div className="feature-icon">{iconLibrary[feature.icon]}</div>
            <h3>{feature.title}</h3>
            <p>{feature.desc}</p>
          </Link>
        ))}
      </div>
    </SectionShell>
  );
}

function ArchitectureSection() {
  const metrics = [
    { value: 'components/*', label: '可复用基础组件层' },
    { value: 'os/*', label: '系统与虚拟化实现层' },
    { value: 'platform/*', label: '平台适配与板级支撑' },
    { value: 'test-suit/*', label: '系统级验证与回归' },
  ];

  const layers = [
    { name: 'Applications & Guests', detail: 'examples / rootfs / guest images' },
    { name: 'ArceOS · StarryOS · Axvisor', detail: '面向不同场景的系统实现路径' },
    { name: 'Shared Components', detail: '内存、调度、虚拟化、驱动、I/O 等复用 crate' },
    { name: 'Platform & Tooling', detail: 'platform / xtask / scripts / board config' },
  ];

  return (
    <SectionShell
      id="architecture"
      className="section-shell--architecture"
      eyebrow="Architecture"
      title="从组件层到系统层，信息结构保持稳定且可推导"
      description="仓库按 components / os / platform / test-suit 四层组织，从基础 crate 到系统实现再到平台适配形成清晰依赖关系。"
      framed={false}
    >
      <div className="split-layout split-layout--architecture">
        <div className="narrative-card">
          <h3>统一工作区不只是把仓库放在一起</h3>
          <p>
            TGOSKits 将共享组件、系统实现、平台适配、测试套件和构建脚本放进同一个演进视角中，
            使“改动会影响哪里”“该从哪个入口验证”这类问题更容易回答。
          </p>
          <div className="metric-strip">
            {metrics.map((metric) => (
              <div className="metric-chip" key={metric.label}>
                <strong>{metric.value}</strong>
                <span>{metric.label}</span>
              </div>
            ))}
          </div>
          <div className="narrative-actions">
            <Link className="button button--primary button--hero button--compact" to="/docs/introduction/overview">
              查看项目概览
            </Link>
            <Link className="button button--outline button--hero button--compact" to="/docs/design/reference/repo">
              浏览仓库结构
            </Link>
          </div>
        </div>
        <div className="stack-visual" aria-hidden="true">
          {layers.map((layer, index) => (
            <div className="stack-layer" key={layer.name} style={{ '--stack-index': index }}>
              <strong>{layer.name}</strong>
              <span>{layer.detail}</span>
            </div>
          ))}
        </div>
      </div>
    </SectionShell>
  );
}

function SystemsSection() {
  const systems = [
    {
      name: 'ArceOS',
      accentClass: 'accent-arceos',
      desc: '模块化内核路径，是多个系统能力向上复用的基础层。',
      items: ['聚焦模块、平台和示例应用', '适合理解基础能力如何组合成系统', '也是 StarryOS 与 Axvisor 的底座之一'],
      to: '/docs/design/systems/arceos-guide',
    },
    {
      name: 'StarryOS',
      accentClass: 'accent-starry',
      desc: '建立在 ArceOS 之上的 Linux 兼容系统，强调内核与 rootfs 联动。',
      items: ['覆盖 syscall、进程、信号等核心语义', '包含 rootfs 与用户态验证路径', '适合完整 OS 路径开发与调试'],
      to: '/docs/design/systems/starryos-guide',
    },
    {
      name: 'Axvisor',
      accentClass: 'accent-axvisor',
      desc: 'Type-I Hypervisor 路径，围绕板级配置、VM 配置和 Guest 镜像组织开发流程。',
      items: ['覆盖 VM、vCPU、虚拟设备与地址空间抽象', '强调虚拟化组件与板级能力协作', '适合系统与虚拟化联合验证'],
      to: '/docs/design/systems/axvisor-guide',
    },
  ];

  return (
    <SectionShell
      id="systems"
      className="section-shell--systems"
      eyebrow="Systems"
      title="三条系统路径，共享组件基础但面向不同开发目标"
      description="ArceOS 提供模块化内核基础，StarryOS 在其上构建 Linux 兼容系统，Axvisor 聚焦 Type-I 虚拟化场景，三者共享组件栈但面向不同目标。"
      framed={false}
    >
      <div className="systems-grid">
        {systems.map((system) => (
          <article className={`system-card ${system.accentClass}`} key={system.name}>
            <div className="system-card__header">
              <h3>{system.name}</h3>
            </div>
            <div className="system-card__body">
              <p>{system.desc}</p>
              <ul>
                {system.items.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
              <Link className="button button--primary button--hero button--compact" to={system.to}>
                进入指南
              </Link>
            </div>
          </article>
        ))}
      </div>
    </SectionShell>
  );
}

function WorkflowSection() {
  const steps = [
    {
      index: '01',
      title: '建立仓库心智模型',
      desc: '先阅读 overview、repo 等文档，明确系统层、组件层和平台层之间的关系。',
      to: '/docs/introduction/overview',
    },
    {
      index: '02',
      title: '跑通最短命令路径',
      desc: '从 quick start 或目标系统指南入手，把本地构建和 QEMU 运行路径先打通。',
      to: '/docs/quickstart/overview',
    },
    {
      index: '03',
      title: '深入设计与验证',
      desc: '进入 architecture、build、test、guest config 等文档，理解底层设计和验证策略。',
      to: '/docs/design/architecture/arch',
    },
  ];

  const commands = [
    'cargo xtask arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf',
    'cargo xtask starry rootfs --arch riscv64',
    'cargo xtask axvisor qemu --arch aarch64',
    'cargo xtask clippy',
  ];

  return (
    <SectionShell
      id="workflow"
      className="section-shell--workflow"
      eyebrow="Getting Started"
      title="首页即入口，阅读顺序与命令顺序相互对应"
      description="从理解项目结构、跑通 QEMU 构建运行，到深入架构设计与验证策略，按顺序渐进式进入开发。"
      framed={false}
    >
      <div className="split-layout split-layout--workflow">
        <div className="workflow-timeline">
          {steps.map((step) => (
            <Link className="workflow-card" key={step.title} to={step.to}>
              <span className="workflow-index">{step.index}</span>
              <h3>{step.title}</h3>
              <p>{step.desc}</p>
            </Link>
          ))}
        </div>
        <div className="command-board">
          <h3>高频命令路径</h3>
          <div className="command-list">
            {commands.map((command) => (
              <code className="command-pill" key={command}>
                {command}
              </code>
            ))}
          </div>
          <div className="command-board__links">
            <Link to="/docs/design/reference/build-system">构建系统说明</Link>
            <Link to="/docs/design/test/overview">验证策略</Link>
            <Link to="/docs/design/build/flow">构建流程</Link>
          </div>
        </div>
      </div>
    </SectionShell>
  );
}

function DocsSection() {
  const docs = [
    {
      title: '项目介绍',
      desc: '先理解仓库目标、系统关系、硬件支持和读者入口。',
      links: [
        { label: '概览', to: '/docs/introduction/overview' },
        { label: '环境与平台', to: '/docs/introduction/hardware' },
      ],
    },
    {
      title: '参考资料',
      desc: '查看仓库结构、组件分析、构建系统和依赖关系等全局性资料。',
      links: [
        { label: '仓库结构', to: '/docs/design/reference/repo' },
        { label: '组件开发指南', to: '/docs/design/reference/components' },
        { label: '构建系统', to: '/docs/design/reference/build-system' },
      ],
    },
    {
      title: '设计与实现',
      desc: '阅读架构、构建链、测试链和 Guest 配置等底层设计说明。',
      links: [
        { label: '架构设计', to: '/docs/design/architecture/arch' },
        { label: '构建流程', to: '/docs/design/build/flow' },
        { label: 'Guest 配置', to: '/docs/design/guest-config/config-overview' },
      ],
    },
    {
      title: '系统指南',
      desc: '按目标系统进入具体开发路径，聚焦目录、命令和验证方式。',
      links: [
        { label: 'ArceOS', to: '/docs/design/systems/arceos-guide' },
        { label: 'StarryOS', to: '/docs/design/systems/starryos-guide' },
        { label: 'Axvisor', to: '/docs/design/systems/axvisor-guide' },
      ],
    },
  ];

  return (
    <SectionShell
      id="docs-map"
      className="section-shell--docs"
      eyebrow="Documentation Map"
      title="文档不只是一串目录，而是一组可组合的阅读入口"
      description="按项目介绍、参考资料、设计与实现、系统指南四个维度组织文档入口，快速跳转到所需层次。"
      framed={false}
    >
      <div className="docs-grid">
        {docs.map((group) => (
          <div className="docs-card" key={group.title}>
            <h3>{group.title}</h3>
            <p>{group.desc}</p>
            <div className="docs-links">
              {group.links.map((link) => (
                <Link key={link.label} to={link.to}>
                  {link.label}
                </Link>
              ))}
            </div>
          </div>
        ))}
      </div>
    </SectionShell>
  );
}

function QualitySection() {
  const lanes = [
    {
      title: 'Host 侧验证',
      desc: '以最小消费者优先，先做组件级标准库测试或 clippy 静态检查。',
      items: ['cargo test -p <crate>', 'cargo xtask test', 'cargo xtask clippy'],
    },
    {
      title: '系统级验证',
      desc: '在目标系统路径中准备镜像、rootfs 或配置，再使用 QEMU 执行最短运行链路。',
      items: ['ArceOS 示例运行', 'StarryOS rootfs + qemu', 'Axvisor setup_qemu + qemu'],
    },
    {
      title: '平台与场景回归',
      desc: '当改动涉及平台、板级配置或跨系统共享能力时，再扩大验证范围。',
      items: ['platform/* 适配检查', 'Guest / VM 配置回归', '多系统共享依赖影响面确认'],
    },
  ];

  return (
    <SectionShell
      id="quality"
      className="section-shell--quality"
      eyebrow="Verification"
      title="从组件到系统再到平台，验证路径与工程层次保持一致"
      description="从 Host 侧组件级测试与静态检查，到 QEMU 系统级运行验证，再到跨平台/跨系统影响面回归，验证粒度与工程层次对齐。"
      framed={false}
    >
      <div className="quality-grid">
        {lanes.map((lane) => (
          <div className="quality-card" key={lane.title}>
            <h3>{lane.title}</h3>
            <p>{lane.desc}</p>
            <ul>
              {lane.items.map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </SectionShell>
  );
}

function PlatformSection() {
  const platformGroups = [
    {
      arch: 'aarch64',
      cssClass: 'aarch64',
      label: 'ARMv8 (AArch64)',
      targets: [
        { name: 'QEMU virt', desc: '虚拟平台仿真', type: 'qemu' },
        { name: 'Raspberry Pi', desc: '树莓派板卡', type: 'board' },
        { name: 'Phytium Pi', desc: '飞腾派板卡', type: 'board' },
        { name: 'BSTA1000B', desc: 'BSTA 板卡', type: 'board' },
      ],
    },
    {
      arch: 'riscv64',
      cssClass: 'riscv64',
      label: 'RISC-V 64',
      targets: [
        { name: 'QEMU virt', desc: '虚拟平台仿真', type: 'qemu' },
      ],
    },
    {
      arch: 'x86_64',
      cssClass: 'x8664',
      label: 'x86-64',
      targets: [
        { name: 'PC (QEMU)', desc: 'x86 PC 平台', type: 'qemu' },
      ],
    },
    {
      arch: 'loongarch64',
      cssClass: 'loongarch64',
      label: 'LoongArch 64',
      targets: [
        { name: 'QEMU virt', desc: '虚拟平台仿真', type: 'qemu' },
      ],
    },
  ];

  return (
    <SectionShell
      id="platforms"
      className="section-shell--platforms"
      eyebrow="Platform Matrix"
      title="从 QEMU 仿真到物理板卡，覆盖主流架构的完整平台矩阵"
      description="平台层不是简单的 BSP 堆叠，而是通过 axplat 体系在统一接口下管理架构差异，并通过 axplat-dyn 支持运行时平台切换。"
      framed={false}
    >
      <div className="platform-matrix">
        {platformGroups.map((group) => (
          <div className={`platform-group platform-group--${group.cssClass}`} key={group.arch}>
            <div className="platform-group__header">
              <span className="platform-arch-badge">{group.arch}</span>
              <strong>{group.label}</strong>
            </div>
            <div className="platform-group__targets">
              {group.targets.map((target) => (
                <div className={`platform-chip platform-chip--${target.type}`} key={target.name}>
                  <span className="platform-chip__name">{target.name}</span>
                  <span className="platform-chip__desc">{target.desc}</span>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
      <div className="platform-footer">
        <div className="platform-footer__note">
          <strong>axplat-dyn</strong>
          <span>动态平台层：支持运行时选择平台实现，无需重新编译即可切换板卡适配。</span>
        </div>
        <Link className="button button--outline button--hero button--compact" to="/docs/introduction/hardware">
          查看完整硬件支持
        </Link>
      </div>
    </SectionShell>
  );
}

function DriverSection() {
  const driverCategories = [
    {
      icon: 'server',
      title: '块设备驱动',
      desc: 'SD/MMC 存储支持',
      cssClass: 'blk',
      items: ['simple-sdmmc'],
    },
    {
      icon: 'chip',
      title: 'NPU 驱动',
      desc: '神经网络加速',
      cssClass: 'npu',
      items: ['rockchip-npu'],
    },
    {
      icon: 'layers',
      title: 'PCI 总线驱动',
      desc: 'PCIe 控制器适配',
      cssClass: 'pci',
      items: ['rk3588-pci'],
    },
    {
      icon: 'grid',
      title: 'SoC 平台驱动',
      desc: '片上系统外设',
      cssClass: 'soc',
      items: ['rockchip (GPIO, clk, reset)'],
    },
  ];

  const driverSubsystems = [
    { name: 'block', label: '块设备' },
    { name: 'display', label: '显示' },
    { name: 'input', label: '输入' },
    { name: 'net', label: '网络' },
    { name: 'pci', label: 'PCI 总线' },
    { name: 'virtio', label: 'VirtIO' },
    { name: 'vsock', label: '虚拟 Socket' },
    { name: 'base', label: '驱动基础层' },
  ];

  return (
    <SectionShell
      id="drivers"
      className="section-shell--drivers"
      eyebrow="Driver Ecosystem"
      title="跨内核可复用的驱动框架，从设备抽象到具体硬件形成统一分层"
      description="驱动不再与单一内核绑定——通过 Driver Core / Capability Boundary / OS Glue / Runtime 四层分离，同一驱动可跨 ArceOS、StarryOS 与 Axvisor 复用。"
      framed={false}
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
                    {cat.items.map((item) => (
                      <span className="driver-tag" key={item}>{item}</span>
                    ))}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </div>
        <div className="driver-subsystem-panel">
          <h3>驱动子系统抽象</h3>
          <p className="driver-subtitle">axdriver_crates 提供的通用驱动接口层</p>
          <div className="driver-subsystem-grid">
            {driverSubsystems.map((sub) => (
              <div className="driver-subsystem-chip" key={sub.name}>
                <code>axdriver_{sub.name}</code>
                <span>{sub.label}</span>
              </div>
            ))}
          </div>
          <div className="driver-framework-note">
            <h4>跨内核驱动框架</h4>
            <p>
              基于 Driver Core → Capability Boundary → OS Glue → Runtime 四层分层模型，
              将驱动核心逻辑与 OS 依赖解耦，通过 mmio-api / dma-api / IRQ 契约实现跨系统复用。
            </p>
          </div>
        </div>
      </div>
    </SectionShell>
  );
}

function CTASection() {
  return (
    <section className="cta-section" id="cta">
      <div className="section-shell__inner">
        <div className="cta-panel">
          <p className="eyebrow">Get Started</p>
          <h2>从统一入口进入 TGOSKits 的系统、组件与工具链世界</h2>
          <p>无论你要做的是系统内核、虚拟化、平台适配，还是共享组件与构建链维护，都可以从首页直接进入对应路径。</p>
          <div className="cta-actions">
            <Link className="button button--primary button--hero" to="/docs/quickstart/overview">
              打开快速开始
            </Link>
            <Link className="button button--outline button--hero" to="/docs/design/reference/components">
              查看组件分析
            </Link>
          </div>
        </div>
      </div>
    </section>
  );
}

export default function Home() {
  const {siteConfig} = useDocusaurusContext();

  return (
    <Layout title={siteConfig.title} description={siteConfig.tagline} wrapperClassName="home">
      <HeroBanner />
      <CapabilitySection />
      <ArchitectureSection />
      <PlatformSection />
      <SystemsSection />
      <DriverSection />
      <WorkflowSection />
      <DocsSection />
      <QualitySection />
      <CTASection />
    </Layout>
  );
}
