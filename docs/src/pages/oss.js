import useVisualHeight from '../hooks/useVisualHeight';
import layout from '../components/layout/page.module.css';
import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import useBaseUrl from '@docusaurus/useBaseUrl';
import useBrokenLinks from '@docusaurus/useBrokenLinks';
import ArceOSArchitecture from '@site/static/images/oss/arceos-architecture.svg';
import AxVisorArchitecture from '@site/static/images/oss/axvisor-architecture.svg';
import StarryArchitecture from '@site/static/images/oss/starry-architecture.svg';
import SystemsOverview from '@site/static/images/oss/systems-overview.svg';
import styles from './oss.module.css';

const systems = [
  {
    id: 'arceos', name: 'ArceOS', summary: '按需组合系统能力', type: '组件化 Unikernel',
    description: '通过 Rust crate、Cargo feature 和目标配置组合系统能力。应用与选定的运行时组件一起构建成镜像，同时为 StarryOS 和 AxVisor 提供可复用的基础运行能力。',
    diagram: ArceOSArchitecture, source: 'os/arceos',
    features: [
      ['应用接口', 'Rust 应用使用 ax-std，C 应用通过 ax-libc 接入；ax-api 与 ax-posix-api 聚合所需的系统接口。'],
      ['运行时装配', 'ax-runtime 组织初始化和任务运行能力，按配置组合内存、文件系统、网络、显示和输入等服务。'],
      ['组件与平台', '任务、分配器、驱动和协议栈分布于共享目录；ax-hal 与 ax-plat 将这些能力接入具体架构和平台。'],
    ],
  },
  {
    id: 'axvisor', name: 'AxVisor', summary: '管理虚拟机与 Guest', type: '组件化 Type-I Hypervisor',
    description: '在 ArceOS 宿主运行能力之上组合虚拟机、地址空间和虚拟设备。由配置描述客户机资源，通过统一管理入口控制 Guest 的创建、运行与退出。',
    diagram: AxVisorArchitecture, source: 'os/axvisor',
    features: [
      ['客户机编排', 'AxvmManager 负责应用层策略，通过 AxvmRuntime 管理虚拟机；shell 和可选 HTTP 接口提供控制入口。'],
      ['虚拟化组件', 'axvm 组合 axaddrspace、axdevice、axvmconfig 及虚拟 I/O 能力，ax-cpu 提供架构相关的 vCPU 执行后端。'],
      ['宿主与设备边界', 'Guest 的设备模型与宿主物理驱动分开维护。ArceOS 提供任务、同步、内存和 HAL 能力，平台配置决定具体资源。'],
    ],
  },
  {
    id: 'starry', name: 'Starry', summary: '运行 Linux 用户态应用', type: 'Linux 兼容操作系统 · StarryOS',
    description: '面向 Linux 用户态程序提供系统调用与进程环境。StarryOS 在 ArceOS 基础能力之上实现文件、内存、信号、网络及资源管理语义，通过 Rootfs 装载并运行应用。',
    diagram: StarryArchitecture, source: 'os/StarryOS', docs: 'starryos',
    features: [
      ['用户态入口', 'ELF 程序和 libc 位于用户态，系统调用与异常由 starry-kernel 的 syscall、trap 等入口接收和处理。'],
      ['内核子系统', 'task、mm、file、ipc、pseudofs、namespace 和 cgroup 等模块维护 Linux 语义；starry-signal 与 starry-vm 提供领域抽象。'],
      ['共享运行能力', '复用 ax-runtime、ax-task、ax-hal 以及文件系统、网络和设备组件；兼容行为由 StarryOS 内核与用户态环境共同验证。'],
    ],
  },
];

function SystemSection({system, index}) {
  // Docusaurus only registers theme heading anchors, so the fragment links from the home page need this section id registered explicitly.
  useBrokenLinks().collectAnchor(system.id);
  const diagramUrl = useBaseUrl(`/images/oss/${system.id}-architecture.svg`);
  const Diagram = system.diagram;
  return <section id={system.id} className={styles.system} data-system={system.id} data-reverse={index % 2 === 1} aria-labelledby={`${system.id}-title`}>
    <div className={`container ${styles.sectionInner}`} data-visual-pair>
      <div className={styles.copy} data-visual-copy>
        <div className={styles.systemHeading}><span className={styles.sectionNumber}>0{index + 1}</span><p className={styles.systemType}>{system.type}</p></div>
        <h2 className={layout.sectionTitle} id={`${system.id}-title`}>{system.name}</h2>
        <p className={styles.description}>{system.description}</p>
        <dl className={styles.features}>{system.features.map(([title, description], featureIndex) => <div key={title}><dt><span aria-hidden="true">0{featureIndex + 1}</span>{title}</dt><dd>{description}</dd></div>)}</dl>
        <div className={styles.actions}><Link className={layout.primaryButton} to={`/docs/quickstart/${system.docs || system.id}`}>快速开始</Link><Link className={layout.secondaryButton} to={`/docs/architecture/${system.docs || system.id}`}>架构文档</Link><Link className={layout.secondaryButton} href={`https://github.com/rcore-os/tgoskits/tree/main/${system.source}`}>源码</Link></div>
      </div>
      <figure className={styles.diagram}>
        <a href={diagramUrl} target="_blank" rel="noopener noreferrer" aria-label={`打开 ${system.name} 完整 SVG 架构图（新窗口）`}><Diagram aria-label={`${system.name} 完整架构图`} /></a>
      </figure>
    </div>
  </section>;
}

export default function OSs() {
  const pageRef = useVisualHeight();
  return <Layout wrapperClassName="site-showcase" title="OSs" description="ArceOS、AxVisor 与 Starry 的系统定位、组件架构与运行链路。">
    <main className={styles.page} ref={pageRef}>
      <header className={styles.hero}>
        <div className={`container ${styles.heroInner}`} data-visual-pair>
          <div className={styles.copy} data-visual-copy>
            <p className={styles.eyebrow}>TGOSKits / OSs</p>
            <h1 className={layout.heroTitle}>三套系统，一体化开发。</h1>
            <p className={styles.description}>从组件化应用到虚拟机，再到 Linux 用户态兼容。在同一工作区中组合共享能力，构建不同的运行环境。</p>
            <dl className={layout.featureList}>
              <div><dt><span aria-hidden="true">01</span>面向不同运行场景</dt><dd>ArceOS 将应用与所需组件构建为系统镜像；AxVisor 编排客户机；Starry 为 Linux 用户态程序提供进程与系统调用环境。</dd></div>
              <div><dt><span aria-hidden="true">02</span>复用基础，独立演进</dt><dd>三套系统共享任务、内存、设备和平台能力，各自维护运行策略与接口语义。功能选择通过 Cargo feature 和目标配置完成。</dd></div>
              <div><dt><span aria-hidden="true">03</span>统一构建与验证</dt><dd>通过 cargo xtask 组织构建、QEMU 运行及板卡测试，让组件开发与系统集成衔接在同一工作区中。</dd></div>
            </dl>
            <Link className={styles.componentLink} to="/components">了解共享组件</Link>
          </div>
          <SystemsOverview className={styles.overviewArt} aria-label="ArceOS、AxVisor 和 Starry 的定位及共享运行基础" />

        </div>
          <nav className={`container ${styles.systemNav}`} aria-label="系统页面导航">{systems.map((system, index) => <a key={system.id} href={`#${system.id}`}><span className={styles.navIndex}>0{index + 1}</span><span><strong>{system.name}</strong><small>{system.summary}</small></span></a>)}</nav>
      </header>
      {systems.map((system, index) => <SystemSection key={system.id} system={system} index={index} />)}
    </main>
  </Layout>;
}
