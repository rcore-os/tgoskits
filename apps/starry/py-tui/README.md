# py-tui — Python TUI 框架地毯式测试

工业级、精确断言（exact-assertion）的终端 UI 框架正确性测试：由 **musl 原生 CPython3** 在 StarryOS
四个架构（x86_64 / aarch64 / riscv64 / loongarch64）上**完全无头（headless）** 运行。leg A 逐控件覆盖
两个 Python TUI 框架 **textual** 与 **casca**；leg B 再以四个真实、流行的 Textualize 应用
（**toolong** 日志查看器、**frogmouth** Markdown 浏览器、**posting** 重量级 HTTP/API 客户端、
**textual-tetris** 俄罗斯方块游戏）端到端反向验证 `textual`。共**六个地毯**。

| 模块 | 框架 | 维度 | 标记 |
|:--|:--|:--|:--|
| textual | Textual | `App.run_test()` + `Pilot` 无头驱动；Static/Label/Button/Input/Checkbox/Switch/RadioSet/Select/ListView/DataTable/Tree/TextArea/ProgressBar/TabbedContent 的构造·渲染·状态·事件往返；`reactive()` + `watch_*` + `compute_*` 互相更新；`BINDINGS`→`action_*`；CSS→`styles.*`（精确 `Color` rgb / 布局 region / display / text-style）；query 选择器（id/type/class/first/last/NoMatches）；屏幕栈 push/pop + modal 查询；`@on` 与自定义 `Message` 派发；键盘→reactive→合成器重绘全链路 | `TEXTUAL_DONE` |
| casca | Casca | 无头 capture surface 记录每次 `draw_text` 成字符网格；Store（reducer/dispatch/subscribe/深拷贝隔离/middleware/错误契约）+ `combine_reducers`；完整 `Keys` 常量表；`KeyEvent`/`MouseEvent`/`ResizeEvent`；ansi 颜色引擎（`Color`/`BackColor`/`color`/`color_from_spec`/`move_cursor`）；CSS 引擎（`parse_css` 简写展开+注释+特异性 / `get_box_spacing` / `parse_size` / `clamp_dimension` / `validate_stylesheet` / `load_css_file`）；主题 `THEMES` + `var()` 解析；插件注册表；以及每个核心控件——Label/Button/Container(flex)/Checkbox/Input/Select/ListView/RadioGroup/ProgressBar/TextArea/Table/Tabs/TreeView/Spinner/Card 的布局·渲染单元格·键鼠控制往返；`App` 生命周期（build_ui/render/set_state/set_store/dispatch/get_state/set_theme/handle_input 路由）+ `run_app` | `CASCA_DONE` |

每个模块都是自包含地毯（独立 ok/fail 计数器），断言全部采用与框架补丁版本无关的**稳定不变量**——
控件结构与状态（计数 / 索引 / 值）、我方设定的精确渲染字符串、CSS 解析出的精确 `Color(r,g,b)`、
reactive 状态转移、store 状态迁移、CLI 输出存在性——因此宿主参照库与（更新的）目标端库会给出逐字相同的
结果，不依赖 TTY、动画、时间戳或随机数。模块仅在内部 fail 计数为 0 时打印其 `*_DONE` 标记；
`run_pytui.py` 运行**六个**地毯，且仅当六个全部通过时才打印 `PY_TUI_OK=6/6` 与 `TEST PASSED`（不允许跳过）。

## 重量级真实应用辅助测试（leg B：toolong + frogmouth + posting + textual-tetris）

除了在隔离环境里逐控件打 textual/casca（leg A），本套件再以**四个真实、流行的 Textualize 应用**端到端、
完全无头地反向验证 `textual` 框架：**toolong**（`tl` 日志查看器）、**frogmouth**（终端 Markdown 浏览器）、
**posting**（12k GitHub 星 终端 HTTP/API 客户端——第三方应用里对 textual 控件/API 面覆盖最重的一个）、以及
**textual-tetris**（真实交互式俄罗斯方块游戏——交互+渲染压力最强的工作负载）。逻辑是——如果一个结构复杂的
真实应用能启动、在后台 worker 线程里扫描/解析文件、正确把合成器输出画出来、并对一整套（乃至上百步的）
键盘交互做出正确响应，那么支撑它们的 `textual` 库就被真实第三方用法证明了。

| 模块 | 真实应用 | 维度 | 标记 |
|:--|:--|:--|:--|
| toolong | toolong 1.5.0（textual 0.58.1） | `UI([fixture.log])` 经 `App.run_test()` 无头启动；后台线程扫描 60 行定长 fixture（INFO/DEBUG/WARNING/ERROR 混合 + 一行纯 JSON）；断言 `line_count`/滚动几何/tail 到底；逐交互重取 `render_strips()` 断言：home/end/pagedown/pageup、方向键单行滚动、边框列对齐（无残帧）、JSON 行经 JSONLogFormat 逐字渲染、`ctrl+l` 行号栏、`enter` 选中指针 + 再 `enter` 行详情面板、`ctrl+g` GotoScreen 屏栈进出、`ctrl+f` 查找 Input 键入回传 `find` reactive；外加 `tl` console-script 结构断言 + `python -m toolong --help`（带硬超时、非阻塞）| `TOOLONG_DONE` |
| frogmouth | frogmouth 0.9.2（textual 0.43.2） | `MarkdownViewer(Namespace(file=[fixture.md]))` 无头启动；把真实 Markdown 解析成 textual `Markdown` 的 20 个区块（H1×1/H2×4/H3×1/表格/围栏代码/无序表/有序表/段落×10）；断言 ToC 收录全部 6 个标题、粗体/斜体/行内代码渲染为纯文本单元；逐交互重断言：Tab 在 Viewer↔Omnibox 间切焦、`ctrl+n` 侧栏开合、滚动露出表格/代码/链接、`scroll_to_block` 跳到末标题（顶标题滚出）、`space/b` 翻页、`ctrl+t` + ToC Tree 方向键选择驱动正文滚动；外加 `frogmouth` console-script 结构断言 + `python -m frogmouth --help`（非阻塞）| `FROGMOUTH_DONE` |
| posting | posting 2.10.0（textual 6.1.0） | `make_posting()` 载入一份提交的 `.posting.yaml` 请求集合，经 `App.run_test(120×40)` 无头启动；一段 **~28 帧的长时交互 soak**：URL 编辑（`ctrl+l`+键入+退格）、集合 Tree 导航（方向键上下）、打开代表性请求（GET `health` + POST `create-user`，断言 URL/方法/请求体/头表载入）、请求编辑器 **8 个 Tab 完整一轮 + 回到 Headers**、请求体 `TextArea` 的 **tree-sitter 实时语法解析**（`SyntaxAwareDocument`+`json` 解析树根 `document`，键入再解析——硬断言，集中在此处而非每帧）、请求搜索面板/跳转模式 的**开→关**往返、命令面板**开→过滤 `theme`→运行主题命令**、集合浏览器开合、区段展开。**每步**只重取一次 `render_strips()`（复用同一快照断言，避免重复渲染）并断言四项渲染不变量：内容对、列对齐（`cell_length==宽`，无残帧/涂抹）、**覆盖层关闭后底屏逐字节复原（无残影）**、静态区（页首）恒定，覆盖 显示/交互/交互后显示 三态。外加 `posting` console-script + click 命令组的**进程内结构断言**（不再子进程 spawn `--help`——见下方性能说明）| `POSTING_DONE` |
| textual-tetris | textual-tetris 0.3.1（textual 8.2.8） | `TetrisApp()` 经 `App.run_test(90×50)` 无头启动；**~99 帧长时对局 soak**：冻结重力定时器、注入已知方块、种子固定 RNG（完全确定）——方块贴墙左右走、四向旋转、软/硬降落锁定+计分+出新块、构造整行触发消行，再长序列连续 移动/旋转/落子跨多个新块。每步断言方块 `.blocks` 坐标与渲染的 `████` 列一致、棋盘框宽恒定（无错位）、锁定格持久、`h` 帮助模态开合底屏无残影；**绝不**触发 game-over/重启（`r`→`os.execl`）| `GAME_DONE` |

### 版本冲突与逐应用隔离

五方 `textual` 互相冲突：leg A 用 8.2.8、toolong 钉 0.58.1、frogmouth 钉 0.43.2、posting 钉 6.1.0、
textual-tetris 用 8.2.8。因此每个真实应用装进**各自独立**的 pip `--target` 目录——`/opt/pytui-toolong`、
`/opt/pytui-frogmouth`、`/opt/pytui-posting`、`/opt/pytui-game`——`run_pytui.py` 为每个地毯子进程设**仅指向
自己那一份**的 `PYTHONPATH`，互不污染。各 `assets/requirements-*.txt` 钉死精确版本 + sha256，纯 Python 部分
全 `py3-none-any`，四架构逐字一致；仓库中**不提交任何 wheel 或二进制**，prebuild 于构建期抓取、哈希校验。
frogmouth 闭包里的 httpx/httpcore/h11/anyio 只是它的**远程 URL** 抓取路径——地毯**只打开本地文件**，运行期不触网。

### posting 的 C 扩展如何在四架构上供给（无 committed wheel/二进制、无跨架构缺口）

posting 是最重的一个，闭包含真正的 C/Rust 扩展。分三层供给，最终都落进 `/opt/pytui-posting`：

1. **纯 Python 闭包**（`requirements-posting.txt`，逐条钉版 + sha256）：宿主 pip `--require-hashes --no-deps
   --target` 抓取，架构无关。
2. **musl 原生 C 扩展——Alpine v3.23 `apk add`，四架构均有预编译、零构建**：`py3-pydantic` + `py3-pydantic-core`
   （成对——纯 pydantic wheel 会钉死一个精确 core 版本，故二者必同源，从 apk 取）、`py3-watchfiles`（Rust）、
   `py3-brotli`（C）、`py3-yaml`（C `_yaml`）。经 qemu-user-static 装入 staging 树后连同 `.so` 闭包复制进 overlay。
   地毯对这几个 apk 供给包只断言**版本稳定不变量**（≥ 下限），不钉精确补丁号（对齐 py-sci）。
3. **tree-sitter 核心 + 15 个语法（`textual[syntax]` 的 TextArea 语法高亮，C；不在 Alpine）**：由 staging 的
   musl pip 在 qemu-user-static 下安装——**有 musllinux wheel 的架构（x86_64 全部、aarch64 多数）直接取 wheel，
   缺的架构（aarch64 少数、riscv64/loongarch64 全部）就地用 apk 装的 `gcc/musl-dev/python3-dev` 从钉版 sdist
   源码构建**（各语法自带的构建后端会正确处理 scanner 与 markdown/xml 的多解析器结构）。版本钉死（无漂移 URL）。

textual-tetris 无任何 C 扩展，全纯 Python，四架构逐字一致（`requirements-game.txt`，11 个 wheel）。

### posting soak 的 on-target 性能（为何刻意精简帧数）

posting 的 widget 树非常大，textual 每次交互要做的 CSS 解析 / 布局 arrange / 消息派发 / URL 输入的逐键
自动补全都是**纯 Python**，在被仿真的 musl 目标上比宿主慢约 1–2 个数量级。剖析证实**瓶颈不是渲染**
（`render_strips()` 仅占宿主墙钟 ~3%，且 tree-sitter 每次重解析只有一步），而是 **pilot 按键/pause 循环
的次数** ——每次交互的重活会被目标端同倍放大。因此本 soak 刻意精简：**仍覆盖每一种交互类型**（URL/方法/
全 8-Tab 循环/树导航/命令面板/搜索/跳转/主题 + 四项渲染不变量的显示·交互·交互后三态），但去掉了冗余键入、
多轮重复、逐步双 pause、以及每个断言各自重渲染（改为每步只渲染一次、复用快照）。初版把整棵树 7 个请求
全开、8-Tab 往复 3 轮、逐字键入长 URL、并子进程 spawn `posting --help`（等于在目标端再整体 import 一遍
重型栈）——这些在宿主上是秒级、在目标端却是灾难级。精简后宿主 soak 墙钟由 ~29s 降到 ~9s、整支 ~13s，
帧数 78→28，断言 173 条全过、三次运行逐字确定。CLI 维度改为**进程内**结构断言（发行入口 + click 命令组），
不再 spawn 第二个解释器。

**根因级修复（目标端能真跑完的关键）——禁用 posting 的文件监视器**：posting 在 mount 时会起三个 `@work`
文件监视器（`watch_env_files` / `watch_collection_files` / `watch_themes`），每个都由 `watchfiles.awatch`
驱动，其 Rust `notify` 后端跑在**独立的 AnyIO 工作线程**里。宿主有 inotify，这些线程休眠；但**目标端没有
inotify**，watchfiles 的 Rust 后端无法挂内核 watch，其线程便**空转（实测 101% CPU）**，饿死 Python 事件
循环——于是 widget 的消息队列**永远排不空**，textual 的 `Pilot._wait_for_screen()` 一直等，最终抛
`WaitForScreenTimeout`（app 连"可测"状态都进不去，等满 900s 也没用）。因 headless 确定性测试根本不需要
实时文件监视，PostingCarpet 在**导入 posting 之前**用 posting 自己的设置（env 前缀 `posting_`）把三个监视器
**全关**：`POSTING_WATCH_ENV_FILES/COLLECTION_FILES/THEMES=false`。**实测**：关掉后三个 watcher worker 与
三个 AnyIO 线程全部消失（线程只剩 `MainThread`、watchfiles 根本不被 import），事件循环得以排空 → posting
在目标端可 settle。carpet 内还加了**自校验断言**（boot 后断言没有 watcher worker、没有 AnyIO 线程残留）
以防回归。宿主行为不变（断言仍全过、逐字确定）。watchfiles 仍作为 posting 的依赖被 provision（存在即可，
运行期不加载）。

作为**兜底**（并非上面死锁的原因）：textual 的 `Pilot._wait_for_screen()` 对屏幕排空消息队列设了 30s 硬上限，
posting 巨型 DOM 在慢目标上仅 boot compose 就可能逼近它；PostingCarpet 在建 `run_test` 前 monkeypatch 把该
上限抬到 **900s**（`PYTUI_SCREEN_TIMEOUT` 可覆盖），让真实慢等待跑完而非提前中止（宿主毫秒级完成、够不到）。
这是 textual 里唯一会抛超时的点——`wait_for_idle()` 是有上限（≤1s）的非抛异常轮询、动画已禁用、
`App.CLOSE_TIMEOUT` 在 textual 6.1.0 里未被使用。

### leg B 的确定性

- 四个应用都跑真实后台加载（toolong 用 `@work(thread=True)` + `mmap` 扫描，frogmouth/posting 用 exclusive
  worker 解析）；地毯以有上限的 `pilot.pause()` 轮询到就绪后才断言，绝不裸 sleep。
- 固定虚拟终端尺寸（toolong 100×30、frogmouth 110×40、posting 120×40、tetris 90×50）；所有 golden 均由**当前
  真实库的实际输出**回算得出；`TEXTUAL_ANIMATIONS=none`/`NO_COLOR`/`TERM=dumb`。
- frogmouth/posting 会把历史/配置写进 XDG 目录：地毯在**导入应用之前**把 `XDG_DATA_HOME`/`XDG_CONFIG_HOME`/
  `HOME` 指向一次性临时目录（posting 还固定 `USER`+`POSTING_HEADING__HOSTNAME` 使页首 `user@host` 确定，
  并把提交的请求集合复制进临时目录以便应用自由保存），每次都从 fixture 干净启动、写入落可丢弃目录，完全可复现。
- posting 全程**不发任何请求**（绝不 `ctrl+j`），运行期不触网。tetris 是天然随机+实时的游戏，地毯**冻结重力
  定时器 + 注入已知方块 + 固定 RNG 种子**把随机与时钟都消除，并**绝不**触发 game-over/重启（`r`→`os.execl` 会
  重执行进程），故每步棋盘坐标与渲染完全确定。

## 无头（headless）与确定性

- **textual** 通过框架自带的 `App.run_test()` 测试驱动运行：它以一个 80×24 的**虚拟终端**驱动真实事件
  循环，不占用 TTY、不进入备用屏幕。`Pilot` 负责按键/点击/暂停并注入事件；断言直接读取合成器的
  strip 文本与控件的 `region`/`size`/`styles`。**绝不**调用会阻塞等待 TTY 的 `App.run()`。
- **casca** 是同步、零依赖框架：控件经 `resolve_styles`→`calculate_layout`→`render(surface)` 渲染进
  进程内 capture surface，交互经 `handle_input(KeyEvent)`/`on_mouse(MouseEvent)` 驱动——全程无终端。

## textual CLI（据实说明 🕊）

核心 `textual` 发行包**不提供** `textual` 命令行入口（该 CLI 属于独立的 `textual-dev` 包，其依赖
`aiohttp`/`msgpack` 等 C 扩展，不符合本测试的纯 Python 可复现模型）；`python -m textual` 存在但它是
**交互式演示程序**，会进入备用屏幕并要求真实 TTY，无法无头且确定性地运行。因此 `run`/`console`/`colors`/
`keys`/`diagnose`/`borders`/`easing` 各子命令在 TextualCarpet 中被记为**有据可查的 SKIP**（并断言核心包
确实不含 `console_scripts` 入口这一结构性事实）。若某构建环境的 `PATH` 上恰好存在真实的 `textual` CLI，
测试**只**以 `--help`（非阻塞、带子进程硬超时）探测各子命令，**绝不**裸跑任何交互子命令。

## 运行

```
cargo xtask starry app qemu -t py-tui --arch x86_64
cargo xtask starry app qemu -t py-tui --arch aarch64
cargo xtask starry app qemu -t py-tui --arch riscv64
cargo xtask starry app qemu -t py-tui --arch loongarch64
```

## 依赖：构建时抓取，不提交任何 wheel

`assets/requirements.txt` 是一份**精确钉版 + sha256 哈希锁定**的清单（textual / casca / rich 及其纯
Python 传递依赖，共 11 个 `py3-none-any` wheel）。仓库中**不提交任何 wheel 或二进制**。`prebuild.sh` 在
构建时执行：

1. 通过 qemu-user-static 把 base Alpine rootfs 解到 staging 树后，将其 apk 仓库指向 Alpine **v3.23**
   分支，`apk add python3 py3-pip`——由 apk 为目标架构解析**当前版本**的 CPython 及其完整 musl `.so`
   闭包（无写死、会漂移的 apk URL，无缓存缺失即退出）。v3.23 为 python 3.12 系列，与 base 镜像同系。
2. `pip install --require-hashes --no-deps -r assets/requirements.txt --target <overlay>/opt/pytui`
   从 PyPI 抓取这份钉版、哈希校验过的闭包。网络是**主路径与唯一真源**；可选的本地 wheel 缓存仅以
   `--find-links` 提速：pip 只会采用 sha256 与钉值一致的缓存 wheel，其余一律回网抓取——缓存缺失
   **绝不**退出。所有 wheel 均为纯 Python（`py3-none-any`），故安装结果与架构无关，四个目标架构逐字
   一致；因此宿主 pip 即可产出对每个目标都可用的 site-packages（回退路径：staging rootfs 内的 musl
   pip 经 qemu-user-static 运行）。
3. 把解释器、其 `.so` 闭包、标准库复制进 per-app overlay；把 `/opt/pytui` 置于运行期 `PYTHONPATH`
   首位，使钉版胜出；地毯源码落 `/root/pytui`，启动器落 `/usr/bin/run-pytui.sh`。

> textual 的可选 `[syntax]` 附加项（tree-sitter，C 扩展）**不纳入**闭包：它仅用于 TextArea 语法高亮，
> 且会破坏纯 Python / 架构无关模型；TextArea 的纯文本能力不受影响。casca 无任何必需运行期依赖。

`prebuild.sh` 在 harness 注入 overlay 之前按需**只增不减**地扩容 per-app rootfs 镜像
（`truncate`+`e2fsck`+`resize2fs`，且 resize 失败不被吞掉），以免 debugfs 注入时静默截断文件。

`programs/run-pytui.sh` 设置 musl 加载器搜索路径、把 `/opt/pytui` 置于 `PYTHONPATH` 首位、固定为
非交互确定性终端环境（`TERM=dumb`/`NO_COLOR`/`COLUMNS=80`/`LINES=24`），然后执行
`python3 /root/pytui/run_pytui.py`。PASS/FAIL 锚点（`TEST PASSED` / `TEST FAILED`）只在 `run_pytui.py`
中打印，启动命令自身绝不打印它，故 success 正则不会自匹配。

## 架构说明

- x86_64：`-cpu Haswell`；`uefi=false` / `to_bin=false`。
- aarch64：`-cpu cortex-a72`（64 位 ARMv8 核心；virt 默认可能以 AArch32 启动导致 64 位内核不引导）。
- riscv64：`-cpu rv64`。
- loongarch64：`-machine virt -cpu la464`，**动态平台**（`build-loongarch64*.toml` 带 `ax-driver/serial`，
  不使用已退休的静态 LoongArch 平台写法）；`uefi=false` / `to_bin=true` 为动态平台的裸二进制引导路径。

四个 `qemu-<arch>.toml` 都用 `-m 2048M`。六个地毯**顺序**跑、各自独立子进程，`posting`（巨型 widget 树）刻意排在最后是峰值内存最高的一个。

## 构建宿主要求（跨架构 tree-sitter 源码构建）

`posting` 依赖 `textual[syntax]` 的 tree-sitter 语法（C 扩展）。非 x86 架构里多数语法在 PyPI 无 musllinux 预编译轮子，需在目标架构下**从源码构建**，`prebuild.sh` 因此要求宿主具备：

- **qemu-user-static + binfmt_misc（`F`/fix_binary 标志）**：把目标架构的 `python3`/编译子进程经模拟器路由（Debian `qemu-user-static` 包安装即注册 aarch64；riscv64/loongarch64 若缺可 `update-binfmts --install` 手动注册，见 `prebuild.sh` 内的守卫报错）。
- **user namespaces（`unshare -r`）**：在 staging rootfs 内以无 root 方式 chroot 运行目标 `pip`（`/lib/ld-musl-<arch>.so.1` 需在 rootfs 内解析，`qemu -L` 不重定向 packaging 对该 loader 的探测）。

语法 sdist 常漏其 `src/tree_sitter/*.h`（乃至 split-parser 的 `common/`），`prebuild.sh` 优先取 musllinux 轮子，无轮子时从该语法**当前 git tag 的完整源码归档**构建（自动从 PyPI 元数据解析 repo，兼容 tree-sitter / tree-sitter-grammars / 其它 org），仓库中**不提交任何 wheel 或二进制**。
