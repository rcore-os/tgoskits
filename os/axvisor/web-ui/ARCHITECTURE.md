# AxVisor Web 控制台前端架构调研

> 本文档是对 `os/axvisor/web-ui/` 前端源码的调研笔记，与 `README.md` 互为补充：
> `README.md` 是设计契约与不变量的权威说明，本文档是「这个目录里每个文件是干什么的」的导览。
> 控制台按 local host 设计，无鉴权、无跨主机安全边界；内核只提供数据与控制接口，
> 界面形态、导航结构与面板划分完全由前端根据内核的 `manifest` 能力声明决定。

## 0. 一句话定位

React + TypeScript + Vite + shadcn/ui 的单页管理台。构建产物在编译期内嵌进 AxVisor 二进制，
运行期不读宿主文件系统。前端与内核之间只有 **HTTP**（REST 控制面）与 **WebSocket**（事件流 + 终端字节流）
两种通道。导航不写死在前端：启动时拉取 `GET /api/manifest`，按返回的面板列表生成左侧导航，
因此「加一个面板」在内核侧加一处声明即可，前端无需改路径常量。

## 1. 目录树与逐文件职责

```
os/axvisor/web-ui/
├── index.html              # 单页入口 HTML，挂载点 #root
├── package.json            # 依赖与脚本(dev/build/typecheck/test)，engines 要求 Node>=24
├── tsconfig.json           # TypeScript 配置，@/ 别名指向 src/
├── vite.config.ts          # Vite 配置（开发服务器把 /api、/ws 代理到 8080）
├── tailwind.config.js      # Tailwind 配置
├── postcss.config.js       # PostCSS(tailwind + autoprefixer)
├── components.json         # shadcn/ui 组件清单
├── README.md               # 架构与不变量的权威说明（本目录唯一事实来源）
├── src/
│   ├── main.tsx            # 入口：把 App 与 panelRegistry 接起来，引入全局样式
│   ├── index.css           # 全局样式 + Tailwind 指令
│   │
│   ├── shell/              # 外壳层：只认识 PanelRegistry 契约，不 import 任何面板实现
│   │   ├── App.tsx         # 读 manifest → 建导航/标签/面板区；错误分类；接入事件源
│   │   ├── Nav.tsx         # 左侧导航：面板列表(来自 manifest) + 实时客户机列表(来自事件通道)
│   │   ├── Tabs.tsx        # 顶部分页：每标签一个面板实例(常驻挂载) + 菜单开新实例
│   │   └── PanelErrorBoundary.tsx  # 每标签的渲染错误边界，失败只降级为一张错误卡片
│   │
│   ├── panels/             # 面板层：每个 kind 一个目录，面板之间绝不互相 import
│   │   ├── registry.ts     # 渲染器注册表 kind→组件(VmsPanel/ConsolePanel/ShellPanel)，懒加载
│   │   ├── FallbackPanel.tsx    # 未知 kind 降级为 JSON 视图(后端可先于前端出新能力，向前兼容)
│   │   ├── vms/VmsPanel.tsx     # 客户机面板：登记表/配置池/目录浏览/生命周期动作
│   │   ├── console/ConsolePanel.tsx  # 客户机终端面板：每通道一个标签，拖拽融合/分离，独占通道管理
│   │   └── shell/ShellPanel.tsx      # 管理台自身 shell(宿主命令解释器，独占通道)
│   │
│   ├── api/                # 接口层：请求/事件/终端/契约类型集中于此，避免各面板各写一份
│   │   ├── types.ts        # 契约类型(Manifest/PanelMeta/VmSummary/VmDetail/PoolInfo/ConsoleInfo…)
│   │   │                   #   + ApiError 与 describeError(区分"没连上后端"与"HTTP 4xx/5xx")
│   │   ├── client.ts       # REST 客户端：相对路径、统一错误解析、useApiClient 单例
│   │   ├── events.ts       # useVmFeed：事件 WebSocket 通道，snapshot/created/removed/status 帧
│   │   ├── ws.ts           # ConsoleSocket：终端 WebSocket，按字符边界分块(4096B 上限)，流式 UTF-8 解码
│   │   ├── client.test.ts  # 客户端单测
│   │   └── events.test.ts  # 事件帧应用逻辑单测
│   │
│   ├── capability/         # 能力层：把 manifest 声明变成访问器(纯函数，可单测)
│   │   ├── manifest.ts     # MANIFEST_PATH('/api/manifest'，前端唯一写死的路径) + proto 版本校验
│   │   ├── accessor.ts     # Capabilities 类：url()/maybeUrl()/bind()，面板只点名动作拿 URL
│   │   └── accessor.test.ts # 访问器单测
│   │
│   ├── lib/                # 基础库：纯函数，状态→文案 / 规则，集中实现并被单测覆盖
│   │   ├── status.ts       # STATUS_TONE：VM 状态到徽章配色(导航与面板共用，避免不一致)
│   │   ├── lifecycle.ts    # settleToTerminalState：动作后轮询直到计数器/状态证明"真生效"
│   │   ├── lifecycle.test.ts
│   │   ├── vcpu.ts         # decodeCpuSet：物理 CPU 亲和性位掩码→CPU id 列表(避免 32 位截断)
│   │   ├── vcpu.test.ts
│   │   ├── lanes.ts        # 终端通道纯函数：guestRoute/lanesToOpen/syncGroups/mergeGroups/splitGroup…
│   │   ├── lanes.test.ts   #   (标签组推导、抢输回退、合并/分离，全为纯函数并单测覆盖)
│   │   ├── panel-error.ts  # classifyPanelFailure：区分"资源取不回"与"契约不符"两种失败
│   │   ├── panel-error.test.ts
│   │   └── utils.ts        # cn()：clsx + tailwind-merge 类名合并
│   │
│   └── components/         # 通用 UI 组件
│       ├── Terminal.tsx    # 单通道 xterm 视图：连接状态/收发计数/重连/独占占用提示/Ctrl+C 复制
│       └── ui/             # shadcn/ui 原子组件(button/badge/card/input/dialog)
└── dist/                   # 构建产物(带内容哈希，不入库；Cargo 开启 web-ui 时整表内嵌)
```

## 2. 分层与关键不变量（改动落点判断依据）

- **外壳 `shell/` 不出现面板 `kind` 字面量**：只通过注册表按 `kind` 取组件，并给每个面板注入一个只绑该面板自身的访问器。新增面板 = 注册表加一行 + 内核 manifest 加一个节点，外壳/导航/客户端都不动。
- **面板之间互不 import**：共享逻辑下沉到 `lib/`；生命周期规则、状态配色、通道分组逻辑都只有一处实现并被单测覆盖。
- **路径只在内核侧声明一次**：面板只向能力访问器点名动作（如 `link.url('start', { id })`），前端唯一的路径常量是引导路径 `/api/manifest`（`capability/manifest.ts` 的 `MANIFEST_PATH`，与内核侧 `MANIFEST_PATH` 成对，静态检查比对）。
- **动作缺失分两类**：本构建本该有却缺失 → 访问器抛错、经面板错误边界显示成错误卡片（不发必然 404 的请求）；只存在于部分构建（`fs` 相关）→ 用 `maybeUrl` 取，`null` 时渲染「这个构建没有配置池」。
- **终端通道独占**：只有当前活动标签的通道才连接；抢输（浏览器只报一次匿名关闭）通过重读 `consoles` 表判断被谁拿走并自动换下一条空闲通道；标签集合由通道表纯函数推导，不得自激重渲染。

## 3. 三条数据通道

| 通道 | 模块 | 语义 |
|------|------|------|
| REST 控制面 | `api/client.ts` + `capability/accessor.ts` | 相对路径请求；错误统一成 `ApiError`（status 0 = 没连上后端） |
| 事件流 WebSocket | `api/events.ts` | `snapshot` 全量 + `created/removed/status` 增量帧；`GET /api/vms` 仍为权威 |
| 终端字节流 WebSocket | `api/ws.ts` | 二进制帧即输出；输入按字符边界分块（网关每帧上限 4096 字节） |

## 4. 构建与运行

前端构建独立于 Cargo（`Cargo` 只读取 `dist/`，不调用 npm）：

```bash
cd os/axvisor/web-ui
npm run build          # tsc --noEmit && vite build → dist/
```

真实启动验证由 QEMU 用例完成（`cargo xtask axvisor test qemu --arch aarch64 --test-case web-ui`），
它既检查静态资源与响应头，也驱动一次真实客户机启动/关闭并验证终端往返。

> 详细的不变量清单（请求失败文案、客户机来自可读目录、通道按需占用、抢输回退、标签由通道表推导、
> 裸 LF 处理、错误边界分类等）见 `README.md` 第 3.2 节，评审时作为硬性要求。
