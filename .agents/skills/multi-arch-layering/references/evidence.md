# 多架构分层方法依据

本文保存“分层共同性上提模式”的研究来源、工程先例和 TGOSKits 源码锚点。它用于解释方法为什么成立，不代替对当前调用链、所有权和各目标真实语义的源码核对。

## 1. 理论基础

项目模式由信息隐藏、共同性与可变性管理、演进式接口设计综合而成。没有任何一项来源要求所有架构必须共享一个 trait，也不能用模式名称代替具体契约分析。

### 1.1 信息隐藏

David L. Parnas 的[On the Criteria to Be Used in Decomposing Systems into Modules](https://doi.org/10.1145/361598.361623) 将模块化的有效性绑定到“按什么准则分解”，并把灵活性与可理解性作为核心目标。对本项目而言，容易随架构变化的寄存器、页表、陷阱和固件决策应封装在所有者模块，上层只依赖当前任务需要的稳定语义。

这篇论文不讨论 Rust trait、条件编译或静态分派。“将 `cfg` 收到装配点”是项目根据 Rust 语言机制作出的工程实现，不是对 Parnas 原文的直接转录。

### 1.2 共同性与可变性

Felix Bachmann 与 Paul Clements 的 SEI 报告[Variability in Software Product Lines](https://www.sei.cmu.edu/library/variability-in-software-product-lines/) 指出，共同核心资产中的可变点需要显式建模并一致处理；不必要可变性、重复实现变化机制和选择不兼容机制都会增加演进成本。Paul Clements 与 Linda Northrop 的书籍[Software Product Lines: Practices and Patterns](https://www.sei.cmu.edu/library/software-product-lines-practices-and-patterns/) 则用共同资产和预先计划的扩展、变体描述软件产品族。

本技能只吸收“显式识别共同性与可变性”的方法，不把 TGOSKits 当作需要预建所有变体的产品线。当前只有一个实现或消费者时，变体管理的成本正是不应提前建立 trait 的原因。

### 1.3 面向对象模式的边界

Gamma、Helm、Johnson 与 Vlissides 的[Design Patterns: Elements of Reusable Object-Oriented Software](https://www.oreilly.com/library/view/design-patterns-elements/0201633612/) 中，Template Method 与“公共算法骨架加可覆盖步骤”相近，Strategy 与“替换底层行为”相近，Bridge 与“隔离上层抽象和底层实现”相近。它们可以帮助命名局部设计力，但不是本项目模式的完整同义词。

这些面向对象模式主要描述运行时组合和类层次。TGOSKits 的目标架构在编译时已经确定，因此优先使用模块选择、关联类型和单态化；套用运行时 Strategy 或 Bridge 反而可能引入不必要的虚表与注册机制。

## 2. 系统先例

成熟系统常在上层提供稳定契约，在构建、模块或显式专用入口中保留平台差异。Linux 和 Rust 的机制证明这种做法可以不依赖运行时对象分派。

### 2.1 Linux 通用实现

Linux Kbuild 的[Generic header files](https://docs.kernel.org/kbuild/makefiles.html#generic-header-files) 允许架构通过 `generic-y` 直接复用 `asm-generic` 头文件，`mandatory-y` 则在架构没有自己的必需头文件时生成通用包装。架构提供同名头文件时保留自己的实现，选择在构建边界完成。

该机制支持“真正共同的默认实现只有一个来源，架构可以保留专有覆盖”，但它不能直接决定 Rust trait 应包含哪些方法。接口形状仍然要从当前消费者和契约归纳。

### 2.2 Rust 平台边界

[Rust Reference 的条件编译说明](https://doc.rust-lang.org/reference/conditional-compilation.html)将 `#[cfg]` 定义为编译期选择：谓词为假的项不进入源码构建结果。Rust 1.95 的[发布说明](https://blog.rust-lang.org/2026/04/16/Rust-1.95.0/)稳定了内置 [`cfg_select!`](https://doc.rust-lang.org/stable/core/macro.cfg_select.html)；它按书写顺序发出第一个谓词为真的分支，可以用在项或表达式位置，未提供 `_` 且没有谓词命中时直接产生编译错误。

截至 2026-09-01，[最新稳定版 Rust 是 1.98](https://blog.rust-lang.org/2026/08/20/Rust-1.98.0/)。本仓库的 [`rust-toolchain.toml`](../../../../rust-toolchain.toml) 锁定 `nightly-2026-07-15`，已经包含稳定后的 `cfg_select!`。创建本技能时，使用 `#![no_std]` 探针分别在 Rust 1.98 和仓库工具链上验证了无功能门使用、表达式位置和首个匹配分支；另一个无 `_`、无匹配分支的探针按预期编译失败。

该宏适合在单一装配点表达“候选后端中只选择一个”，但不自动证明谓词互斥。多个功能开关可以同时为真，首个匹配语义可能掩盖重叠；除非顺序就是明确的优先级契约，否则仍需显式拒绝重叠。独立能力需要同时进入构建、需要在项上附着条件或最低 Rust 版本早于 1.95 时继续使用 `#[cfg]`。`cfg!` 只计算布尔值，不移除其他目标无法类型检查的代码，不能替代这两种装配机制。

Rust 标准库的 [PAL 装配源码](https://github.com/rust-lang/rust/blob/main/library/std/src/sys/pal/mod.rs) 使用 `cfg_select!` 选中一个 Unix、Windows、Wasm 或其他目标后端，再重新导出该后端。

[RFC 517](https://github.com/rust-lang/rfcs/blob/master/text/0517-io-os-reform.md) 进一步要求跨平台接口只暴露所有平台都支持且语义可以保持大致等价的服务；无法低成本形成等价语义的功能使用显式平台专用入口。这与“底层差异不伪装成可移植能力，继续向上寻找共同领域动作”相互印证。

## 3. 项目先例

TGOSKits 已经同时存在正面和反面锚点。当前代码会持续演进，因此应在每次使用技能时重新检查符号和调用链，不把本文的路径当作永久不变的事实。

### 3.1 AxVM 分层

[AxVM 分层能力接口设计](../../../../docs/design/axvm-capability-layering.md) 是虚拟化领域的权威基线。`virtualization/axvm/src/architecture/mod.rs` 中的 `Architecture` 组合 `ArchOps`、`MachinePlatform`、`GuestBootPlatform` 与 `BootImagePlatform`，而不是重新声明所有方法。`virtualization/axvm/src/arch/current.rs` 把 `CurrentArch` 与关联类型绑定到当前目标，通用运行期不需要运行时架构分派。

`virtualization/axvm/src/architecture/cpu_up.rs` 中的 `CpuUpOps` 表达只有部分架构拥有的能力，共同处理器启动流程放在通用 `handle()` 中，默认成功返回操作可被寄存器约定不同的架构覆盖。这是“部分能力、共同机制、特异策略”的直接实例。

### 3.2 平台能力边界

`platforms/someboot/src/lib.rs` 中的 `SystimerArch` 只由硬件上拥有独立系统定时器的架构实现；x86_64 由本地高级可编程中断控制器拥有定时器，因此不提供伪 `SystimerArch` 实现。该 trait 的默认方法承载通用中断线和时间转换算法，LoongArch 可为多中断线 ECFG 语义覆盖必要方法。

同一文件的 `ArchTrait` 则同时包含地址转换、页表、陷阱、对称多处理启动、定时器、中断、缓存、直接内存访问一致性和固件入口。它是检查过宽 trait 和缺失能力伪默认值的现实候选，但不能只根据方法数量机械拆分；拆分仍需要完整追踪调用者、初始化顺序和状态所有权。

## 4. 解释边界

上述来源只支持核心判断：封装真实变化点、显式管理共同性与可变性、在构建边界选择后端、将无法保持等价语义的操作留在专用入口。五层结构、两个实现后再抽象、默认方法的覆盖规则和 `CurrentArch` 组装方式是结合 TGOSKits 现状形成的项目工程规则。

使用时不应援引某篇论文或成熟项目作为不读当前源码的理由。如果多架构实现的安全、失败、顺序、性能或所有权契约与先例不同，以当前项目事实为准，并在设计中明确说明为什么停止上提。
