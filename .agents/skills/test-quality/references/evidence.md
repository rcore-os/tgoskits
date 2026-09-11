# 测试质量方法论依据

本文保存 `test-quality` 的术语来源、工程经验和实证研究。只有用户要求解释测试方法论、比较定义或给出书籍与论文依据时才读取；普通测试设计和审查直接使用 `SKILL.md` 的决策规则。

## 1. 术语与分类

测试层级在行业中没有唯一物理边界，因而技能把目标范围和执行条件分开记录，而不以框架、目录、进程数或是否使用模拟对象单独定名。

- [ISTQB Foundation Level Syllabus 4.0.1](https://www.istqb.org/wp-content/uploads/2024/11/ISTQB_CTFL_Syllabus_v4.0.1.pdf) 将 component testing 定义为隔离组件测试，将 component integration testing 定义为组件接口与交互测试，将 system integration testing 定义为本系统与其他系统或外部服务的接口测试。
- Martin Fowler 的 [Unit Test](https://martinfowler.com/bliki/UnitTest.html) 区分 solitary 与 sociable unit test；[Integration Test](https://martinfowler.com/bliki/IntegrationTest.html) 说明该术语在不同团队中的范围并不一致。
- [Software Engineering at Google，第 11 章](https://abseil.io/resources/swe-book/html/ch11.html) 把测试范围（scope）与测试规模（size）分开：前者表示验证的代码和行为范围，后者表示进程、网络、资源和时间成本。
- [Pact 官方文档](https://docs.pact.io/) 将契约测试定义为分别检查集成双方发送或接收的消息是否符合共同契约；它不要求两个完整系统同时部署，也不证明提供方的全部业务副作用。

## 2. 书籍与工程经验

这些来源共同支持测试行为、可维护性、确定性和成本判断，但其组织规模、语言和设计流派不同，不应机械转化为固定比例。

- [Software Engineering at Google，第 11 章](https://abseil.io/resources/swe-book/html/ch11.html) 强调测试不仅发现缺陷，也支持安全变更；测试应清楚、可诊断并尽量自包含隔离（hermetic）。
- [第 12 章](https://abseil.io/resources/swe-book/html/ch12.html) 建议通过公共接口测试行为而不是逐个测试方法，并把与无关重构一起破裂的测试视为脆弱测试。
- [第 13 章](https://abseil.io/resources/swe-book/html/ch13.html) 说明真实实现提高保真度，测试替身（test double）提高隔离性；过度使用模拟对象容易与实现耦合并和真实依赖漂移。
- [第 14 章](https://abseil.io/resources/swe-book/html/ch14.html) 从自包含隔离性、保真度、维护与运行成本讨论大型测试，不把系统测试视为单元测试的替代品。
- Gerard Meszaros 的 [xUnit Test Patterns](https://www.informit.com/store/xunit-test-patterns-refactoring-test-code-9780321504807) 把易运行、易理解、降低风险和随系统演进保持低维护成本列为测试自动化目标，并系统整理测试异味。
- Kent Beck 的 [Test-Driven Development: By Example](https://www.pearson.com/en-us/subject-catalog/p/test-driven-development-by-example/P200000009421/9780321146533) 提供测试优先、隔离测试、测试清单和回归测试模式，但主要讨论测试驱动开发，不是完整系统测试策略。
- Michael Feathers 的 [Working Effectively with Legacy Code](https://objectmentor.com/resources/articles/WorkingEffectivelyWithLegacyCode.pdf) 使用 seam 建立可替换、可观察的边界，让遗留代码能够先获得行为保护再渐进修改。
- Steve Freeman 与 Nat Pryce 的 [Growing Object-Oriented Software, Guided by Tests](https://growing-object-oriented-software.com/toc.html) 明确主张测试行为而不是方法，并区分进度测试、回归测试、单元协作与端到端验收。
- Fowler 的 [Mocks Aren't Stubs](https://martinfowler.com/articles/mocksArentStubs.html) 指出交互测试更容易耦合实现，同时也承认模拟对象在复杂测试夹具和无法直接观察的交互契约中有价值。
- [The Practical Test Pyramid](https://martinfowler.com/articles/practical-test-pyramid.html) 建议将能够以同等真实性在低层证明的场景下移，并删除不再提供价值的高成本重复测试。
- [Google Testing Blog 的测试金字塔文章](https://testing.googleblog.com/2015/04/just-say-no-to-more-end-to-end-tests.html) 把 70/20/10 明确描述为初始猜测而非通用标准；实际组合应由产品风险和架构决定。
- [Jest Snapshot Testing](https://jestjs.io/docs/30.0/snapshot-testing) 要求快照（snapshot）短小、确定、可审查，更新前必须确认差异属于预期行为变化。
- [Understanding Your Coverage Data](https://testing.googleblog.com/2008/03/tott-understanding-your-coverage-data.html) 指出高覆盖率是良好测试的必要但不充分条件；执行代码不等于正确验证代码。

## 3. 实证研究

这些研究为缺陷敏感度、变异测试、不稳定测试、模拟对象和测试代码缺陷提供证据。多数样本来自 Java、开源仓库或单一公司，使用时必须同时说明研究范围和外部有效性限制。

- [Test case quality: an empirical study on belief and evidence](https://arxiv.org/abs/2307.06410) 从既有 29 个开发者假设中选择 8 个，在 42 个成熟 Java 开源仓库上分析测试历史，只找到极弱的支持证据。结论是常见静态特征不足以单独预测缺陷发现能力，不是这些工程建议已经被全部证伪。
- [Long Term Effects of Mutation Testing](https://research.google/pubs/long-term-effects-of-mutation-testing/) 分析 Google 六年中约 1,473 万个变异体，发现接触变异测试的开发者会增加测试并降低后续变异体存活率。该观察性研究来自单一公司，变异分数不能取代真实风险与故障模型。
- [An empirical analysis of flaky tests](https://experts.illinois.edu/en/publications/an-empirical-analysis-of-flaky-tests/) 分析 51 个 Apache 项目中的 201 个修复提交，主要根因包括异步等待、并发和测试顺序依赖；识别出的不稳定测试中 78% 在最初加入时已经不稳定。研究只覆盖已修复且可检索的 Apache 案例。
- [An Empirical Study of Bugs in Test Code](https://people.ece.ubc.ca/amesbah/resources/papers/icsme15.pdf) 从 211 个 Apache 项目中收集 5,556 个测试相关缺陷，并人工分析 443 个；约 97% 会产生错误告警，约 3% 会在产品错误时保持绿色，后者中 67% 与错误或缺失断言有关。分母是已报告且已修复的测试缺陷，不代表所有测试错误。
- [Mock objects for testing Java systems](https://link.springer.com/article/10.1007/s10664-018-9663-0) 研究三个开源 Java 系统、一个工业系统并调查 105 名开发者，说明模拟对象有助于隔离昂贵依赖，但会降低现实性并随生产接口或内部实现变化而维护。四个系统的样本不能泛化为所有语言和架构。

## 4. 项目选择与解释边界

项目以最少且充分的测试证明完整通用功能。以下区分外部资料支持的方法和项目自行确定的约束，避免把工程选择写成书籍的普遍结论。

### 4.1 行为与组织

[Software Engineering at Google，第 12 章](https://abseil.io/resources/swe-book/html/ch12.html) 的 “Test Behaviors, Not Methods” 和 “Test via Public APIs” 支持按行为组织、减少实现耦合；书中仍使用具体输入，也保留无效交易不改变余额的验证，并未主张删除所有错误输入测试。[The Practical Test Pyramid](https://martinfowler.com/articles/practical-test-pyramid.html) 支持删除没有额外价值的跨层重复证明，不要求把所有测试都变成端到端测试。

[The Rust Programming Language：Test Organization](https://doc.rust-lang.org/book/ch11-03-test-organization.html) 说明源码内 `#[cfg(test)]` 单元测试可访问私有接口，`tests/` 集成测试作为外部消费者使用公开接口。书中描述语言机制与常见组织方式，不意味着每个私有函数都需要测试。

### 4.2 项目约束

单元测试固定放在对应源码末尾、禁止测试通过 `#[path]` 或其他源码包含方式引入生产实现，是本项目为保持正常编译与接口边界选择的约束。删除独立参数回读和配置清单镜像，将有实际后果的输入拒绝并入完整功能验证，也是项目组织规则；这些要求不应归因于书籍禁止固定数值或所有边界测试。

项目要求错误修复具备确定性红绿证明，但优先复用或增强已有通用功能测试，不要求每个历史问题新增用例。去重不能丢弃独特失败信号，也不把零测试拒绝、LTP 完成判定或实际架构执行当作冗余参数检查撤销。

上述来源不能推出固定测试比例、覆盖率阈值、变异分数或“永远不用模拟对象”的规则，也不能证明有限样例覆盖所有输入。本项目不强制使用随机生成或属性测试框架；选择样例和测试层级仍取决于功能规则、独特错误信号及运行成本。
