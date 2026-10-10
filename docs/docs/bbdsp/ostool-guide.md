---
sidebar_position: 3
sidebar_label: "ostool 使用"
---
# ostool 使用

`ostool` 是面向操作系统开发的构建和运行工具。它在本机准备内核产物，再通过 `ostool-server` 申请远程开发板、传输启动文件、控制启动流程并转发串口。使用公网租赁平台时，客户端还要先完成用户登录，平台再按有效租约决定是否允许创建板卡会话。

本文先说明 `ostool` 的工作边界和配置文件，再以 Banana Pi BPI-SM10 运行 StarryOS 为完整示例。TGOSKits 对开发板命令的封装见[板卡管理](../build/board.md)。

## 1. 工作方式

`ostool` 同时支持 QEMU、本地 U-Boot 串口和远程开发板。本文主要使用 `ostool board` 命令组，它把本地的构建产物与远程板卡会话连接起来。

### 1.1 客户端与服务端

`ostool` 命令行客户端运行在开发者电脑上，`ostool-server` 运行在连接实体开发板的服务器上。客户端负责读取项目配置、准备内核和发起请求；服务端负责分配板卡、向板卡提供临时文件、控制电源或启动程序，以及在客户端与串口之间转发数据。

一次 `ostool board run` 的主要流程如下。构建和板卡运行是两个独立阶段，因此需要两份配置。

```mermaid
flowchart LR
    A["读取构建配置"] --> B["构建或定位内核产物"]
    B --> C["读取板卡运行配置"]
    C --> D["向 ostool-server 申请会话"]
    D --> E["传输内核并启动板卡"]
    E --> F["转发串口并检查运行结果"]
    F --> G["退出并释放会话"]
```

`board connect` 只执行会话申请和串口连接，不准备或启动新内核。`board run` 则执行图中的完整流程，适合反复构建和验证操作系统。

### 1.2 配置文件

`ostool` 把长期服务配置、单次内核构建方式和板卡启动方式分开保存。这三类配置的作用范围不同。

| 配置 | 典型路径 | 作用 |
| --- | --- | --- |
| 全局板卡服务配置 | `~/.ostool/config.toml` | 保存默认 `server`、`port` 和 `auth_mode` |
| 构建配置 | `.build.toml` 或 `--config <PATH>` | 定义怎样构建内核，或者到哪里取已构建的 ELF |
| 板卡运行配置 | `.board.toml` 或 `--board-config <PATH>` | 定义 `board_type`、加载地址、启动命令、超时和成败判定规则 |

`board run` 默认读取 Cargo 工作区根目录的 `.build.toml` 和 `.board.toml`，命令行显式传入的 `--config` 和 `--board-config` 优先于这两份默认文件。`--board-type`、`--server` 和 `--port` 又可以覆盖板卡运行配置或全局配置中的同名值。实际项目应优先保存配置文件，只把临时变更放在命令行中。

### 1.3 命令分组

`ostool` 的命令按构建、本地运行、远程板卡和认证分组。命令共用同一套构建产物准备逻辑，但运行器不同。

| 命令 | 用途 |
| --- | --- |
| `ostool build` | 只按构建配置生成或准备内核产物 |
| `ostool run qemu` | 构建后使用 QEMU 配置在本机运行 |
| `ostool run uboot` | 构建后通过本地串口与 U-Boot 运行 |
| `ostool board ls` | 查询服务端可申请的板卡类型 |
| `ostool board connect` | 申请板卡并打开串口，不部署新内核 |
| `ostool board run` | 准备内核、申请板卡、启动并检查结果 |
| `ostool board config` | 编辑客户端的默认板卡服务地址 |
| `ostool login`、`ostool auth status`、`ostool logout` | 管理需要认证的板卡服务凭据 |

`ostool menuconfig` 用于交互式编辑默认配置。脚本和可复现的项目流程应显式传入已经审查的 TOML 文件，避免依赖某台开发机上的交互式选项。

### 1.4 板卡命令参数

本文使用的 `ostool 0.29.5` 把构建选项和板卡选项同时放在 `board run` 下。下表列出 `ostool board` 流程中可能出现的命令行参数。

| 参数 | 适用命令 | 含义 |
| --- | --- | --- |
| `-m, --manifest <PATH>` | `ostool` 全局 | 指定用于确定工作区的 Cargo manifest；在 TGOSKits 根目录执行时通常不需要 |
| `-c, --config <PATH>` | `board run` | 指定构建配置；省略时读取工作区根目录的 `.build.toml` |
| `--package <NAME>` | `board run` | 覆盖软件包名，仅适用于 `system.Cargo` 构建配置；`Custom`（预构建）配置下传入会让命令报错退出 |
| `--bin <NAME>` | `board run` | 选择 binary target，仅适用于 `system.Cargo` 构建配置，不能与 `--test` 同时使用；`Custom`（预构建）配置下传入会让命令报错退出 |
| `--test <NAME>` | `board run` | 选择 test target，仅适用于 `system.Cargo` 构建配置，不能与 `--bin` 同时使用；`Custom`（预构建）配置下传入会让命令报错退出 |
| `--board-config <PATH>` | `board run` | 指定板卡运行配置；省略时读取 Cargo 工作区根目录的 `.board.toml` |
| `-b, --board-type <TYPE>` | `board run`、`board connect` | 选择服务端登记的板卡类型；在 `board run` 中会覆盖 TOML 内的 `board_type`，在 `board connect` 中为必需参数 |
| `--board-id <ID>` | `board connect` | 在所选板卡类型中指定一块开发板；省略时由服务端分配 |
| `--server <URL>` | `board ls`、`board connect`、`board run` | 临时覆盖服务地址；可包含协议和端口 |
| `--port <PORT>` | `board ls`、`board connect`、`board run` | 临时覆盖服务端口；也会覆盖 `--server` URL 中已写的端口 |

`board run` 中服务端点的优先级为命令行、板卡运行配置、`~/.ostool/config.toml`。只在需要临时切换服务或板型时使用覆盖参数，固定流程应使用 TOML 保存配置。

## 2. 安装并配置客户端

使用租赁平台前，需要在本机安装 `ostool`，并把客户端指向管理员提供的平台 HTTPS 地址。本文已验证的组合是 `ostool 0.29.5` 与平台服务端 `ostool-server 0.6.0`。两者独立发布，版本号不必相同。客户端不强制固定为 `0.29.5`，但使用其他版本前，应确认配置格式及平台接口兼容；后续版本的兼容性尚未验证。服务端由平台管理员维护。

### 2.1 安装客户端

以下命令安装本文已验证的版本，安装后用 `ostool --version` 确认当前客户端版本。

```bash
cargo install ostool@0.29.5
ostool --version
```

`cargo install` 会安装独立的 `ostool` 可执行文件。如果 shell 仍找到旧版本，检查 `command -v ostool` 指向的路径是否为 Cargo 的可执行文件目录。

### 2.2 配置公网服务

配置界面只提供 `server` 和可选的 `port`，不能修改认证模式。新建配置的认证模式默认为 `disabled`，因此还要手工把 `auth_mode` 改为 `required`。

```bash
ostool board config
```

可以先在 TUI 中把 `server` 设为 `https://ostool.muxai.net:14321/`，保存后再编辑 `~/.ostool/config.toml` 补上认证模式；也可以跳过 TUI，直接编辑文件中的两个字段。最终配置应为以下内容，URL 已经包含端口，因此 `port` 字段可以省略。

```toml
[board]
server = "https://ostool.muxai.net:14321/"
auth_mode = "required"
```

公网服务必须使用 HTTPS。客户端通过系统信任库校验证书；遇到证书错误时，应检查系统时间和证书信任，不要改成 HTTP 或跳过校验。

## 3. 登录与凭据

`ostool login` 默认使用浏览器设备授权。客户端会显示授权地址和一组短期用户码，并等待浏览器确认；授权成功后才会保存登录凭据。用户不应把用户码、设备码或令牌复制到文档、截图和聊天中。

### 3.1 完成浏览器授权

配置完成后，在终端启动登录。管理员后台账号不能代替普通用户账号完成这一步。

```bash
ostool login
```

终端会打印授权网址和 `Code`。按下面的顺序操作，保留当前命令等待授权结果：

1. 打开终端给出的授权网址。
2. 使用平台普通用户的邮箱和密码登录。
3. 核对页面与终端显示的用户码，然后确认授权。
4. 返回终端，等待 `Logged in successfully.`。

设备码由客户端在后台换取凭据，用户码只用于浏览器确认。两者都有有效期；页面提示无效、过期或已经使用时，重新执行 `ostool login`，使用新一组授权信息。

<!-- 截图占位：浏览器授权页面或等待授权的终端。建议路径：/img/build/development-board-rental/ostool-browser-authorization.png。加入截图前，使用 *** 遮盖用户码、设备码、用户名、邮箱和任何 token，不要保留可识别的开发板或租约信息。 -->

授权成功后，OAuth access token 到期时 `TokenManager` 会使用刷新凭据自动续签。凭据优先保存在系统 credential store；系统不支持时，`ostool` 会给出警告并退回用户级凭据文件。配置文件中不要写 token，也不要在截图中展示凭据文件内容。

### 3.2 检查状态并退出

状态命令用于确认服务地址、认证模式和凭据类型。首次登录前通常会看到 `auth_mode: Required` 和 `credential: none`；浏览器授权成功后，`credential` 应为 `OAuth`，还可能显示过期时间和权限范围，但不会打印 token 本身。

```bash
ostool auth status
```

使用结束后可以退出 CLI 登录。退出会清除客户端保存的登录凭据，但不会结束管理员创建的租赁记录。

```bash
ostool logout
```

再次执行 `ostool auth status` 时，已保存的浏览器登录凭据应显示为 `credential: none`。如果后续还要连接公网开发板，需要重新执行 `ostool login`。

## 4. 租赁与开发板会话

登录只证明客户端取得了当前用户的身份凭据，不代表账号已经获得开发板。网页首页的“立即申请”目前不能创建租赁，管理员必须先在后台为该普通用户建立一条当前有效的租赁。这是当前租赁平台的部署行为，不是 `ostool` 对所有服务端的通用要求。

### 4.1 确认有效租约

平台创建会话时会检查当前用户、请求的 `board_type`、标签、租赁状态和有效期。注册账号、登录网页或看到首页的空闲开发板，都不能替代管理员创建的有效租赁。

取得租赁后，可以列出当前服务返回的开发板类型。后续命令中的 `<开发板类型>` 应使用这里显示的 `board_type`，不要填网页上的开发板 ID。

```bash
ostool board ls
```

如果列表中没有需要的类型，或创建会话时报“无对应开发板的租赁权限”，请管理员核对租赁用户、开发板型号、标签、状态和起止时间。有效租赁是准入权限，不等同于已经占用一块实体开发板。

### 4.2 连接串口

`board connect` 按 `--board-type` 请求一块匹配的开发板。分配成功后，终端会显示开发板型号、开发板 ID、会话 ID 和到期时间；开发板提供串口时，命令随后进入 `ostool` 串口终端。

```bash
ostool board connect --board-type <开发板类型>
```

退出方式取决于当前活动界面。界面是 `ostool` 串口终端且启用了退出序列时，使用 `Ctrl+A`，松开后再按 `x`；这种界面既可能来自直接运行的 `ostool board connect`，也可能出现在 `cargo xtask board connect`，以及由 `ostool` 承载的 U-Boot 或 HTTP Boot 终端中。该按键序列不适用于 QEMU 或任意其他命令行提示符，遇到其他界面时应按对应文档或当前提示退出。不要强制结束管理开发板会话的进程，否则客户端可能来不及请求释放会话。

会话创建后，`ostool` 会自动发送保活请求。正常退出串口终端时，客户端会停止保活并尝试释放会话。释放会话不会删除或结束管理员维护的租赁。

## 5. StarryOS 板卡启动

本示例使用独立的 `ostool` 客户端部署已构建的 StarryOS 内核，分别检查构建产物和部署过程。第 5.4 节同时记录 `cargo xtask starry board` 入口及当前 `bananapi` 分支的已知限制。

### 5.1 获取源码并构建

克隆 TGOSKits 后切换到 `bananapi` 分支。示例中的 `bananapi-bpi-sm10.toml` 是 StarryOS 构建配置，它选择 RISC-V 目标、驱动功能和 CPU 数量，不是 `ostool` 的板卡运行配置。

该板卡配置目前位于 `bananapi` 分支，默认分支 `dev` 尚未包含它。示例以该分支为来源；板卡支持合入 `dev` 后，需要同步更新本文的分支选择说明。

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
git checkout bananapi
cargo xtask starry build \
  --config os/StarryOS/configs/board/bananapi-bpi-sm10.toml
```

构建成功时，任务工具会打印 `[axbuild] starry artifact elf=...`，该路径是下一步要使用的 StarryOS ELF。如果没有看到这行或命令返回非零状态，先处理构建错误，不要继续申请板卡会话。

### 5.2 准备运行配置

在 TGOSKits 仓库根目录创建 `tmp` 目录，然后按下文内容保存两份 TOML 文件。这里的 `./tmp` 是仓库内的相对路径，不是系统目录 `/tmp` 或仓库上一级的 `../tmp`。

```bash
mkdir -p tmp
```

将以下内容保存为 `tmp/starry-bananapi-prebuilt.toml`。`build_cmd = "true"` 直接返回成功，使 `ostool` 使用已构建的 ELF；`elf_path` 相对于仓库根目录，`to_bin = true` 显式准备原始 BIN 产物。

```toml
[system.Custom]
build_cmd = "true"
elf_path = "target/riscv64gc-unknown-none-elf/release/starryos"
to_bin = true
```

将以下内容保存为 `tmp/starry-bananapi-board.toml`。该配置申请 `BananaPi` 类型的板卡，并在 StarryOS shell 就绪后发送命令，通过完整的启动标记判断成功。

```toml
board_type = "BananaPi"
kernel_load_addr = "0x102200000"
fit_load_addr = "0x140000000"
bootm_addr = "0x140000000"
fail_regex = [
  "(?i)\\bpanic(?:ked)?\\b",
  "(?i)segmentation fault",
  "(?i)SIGSEGV",
  "exit with code: 139",
  "failed to determine root device",
]
timeout = 600

[[shell_check_steps]]
shell_prefix = "root@starry:"
shell_cmd = "echo STARRY_BPI_SM10_BOOT_OK"
success_regex = ["(?m)^(?:\\x1b\\[[0-9;]*m)*STARRY_BPI_SM10_BOOT_OK\\s*$"]
```

成功正则允许标记前出现 ANSI 颜色控制序列，例如日志中的 `ESC[mSTARRY_BPI_SM10_BOOT_OK`。`ShellCheckMatcher` 对串口文本进行正则匹配，不会先删除这些控制序列；只使用 `^STARRY_BPI_SM10_BOOT_OK` 会漏掉带颜色重置前缀的输出。这里仍要求独立一行的完整标记，避免把命令回显当作执行成功。

两份配置分别控制本地产物和远程启动，不能互换。

| 文件 | 命令行参数 | 职责 |
| --- | --- | --- |
| `starry-bananapi-prebuilt.toml` | `--config` | 使 `ostool` 使用上一步产生的 ELF，避免用另一套参数重新编译 StarryOS |
| `starry-bananapi-board.toml` | `--board-config` | 指定 `board_type`、内核加载地址、U-Boot 启动方式、超时以及成败判定规则 |

`starry-bananapi-prebuilt.toml` 使用 `Custom` 构建系统接入已有产物。这份文件的字段含义如下。

| 字段 | 是否必需 | 含义 |
| --- | --- | --- |
| `[system.Custom]` | 是 | 选择 `Custom` 构建系统的 TOML 表头 |
| `system.Custom.build_cmd` | 是 | `ostool` 准备产物前执行的 shell 命令；本例使用 `true` 跳过编译 |
| `system.Custom.elf_path` | 是 | 已构建的内核 ELF 路径；在本示例中应指向 `cargo xtask starry build` 打印的产物 |
| `system.Custom.to_bin` | 否 | 是否显式把 ELF 转换为原始 BIN；本例设为 `true` |

`elf_path` 必须与构建日志打印的 ELF 一致。切换分支、目标或构建模式后，需要重新检查这个字段；否则 `ostool` 可能启动旧产物，或在上传前报文件不存在。

`starry-bananapi-board.toml` 由 `BoardRunConfig` 加载。Banana Pi 示例只需要其中一部分字段，但了解全部字段有助于排查板卡差异。

| 字段 | 是否必需 | 含义 |
| --- | --- | --- |
| `board_type` | 是 | 向服务端申请的板卡类型，必须与 `ostool board ls` 返回的名称一致 |
| `session_files` | 否 | 随本次会话上传的附加文件；相对路径以板卡 TOML 所在目录为基准 |
| `dtb_file` | 否 | 客户端上传的设备树文件；服务端的板卡配置已提供 DTB 时可省略 |
| `kernel_load_addr` | 否 | U-Boot 把内核加载到内存的地址 |
| `fit_load_addr` | 否 | U-Boot 把 FIT 镜像加载到内存的地址 |
| `bootm_addr` | 否 | `bootm` 命令使用的 FIT 镜像地址，通常与 `fit_load_addr` 一致 |
| `fail_regex` | 否 | 在串口输出中立即判定失败的正则表达式列表，可用于识别 panic、段错误或启动错误 |
| `uboot_cmd` | 否 | 覆盖默认 U-Boot 启动流程的命令列表；只在板卡需要特殊命令时设置 |
| `shell_check_steps` | 否 | 按顺序等待 shell 提示符、发送命令并检查输出的步骤列表 |
| `timeout` | 否 | 等待启动和运行结果的最长时间，单位为秒 |
| `auth_mode` | 否 | 覆盖全局认证模式，`disabled` 不携带凭据，`required` 要求 HTTPS 和有效凭据 |
| `server` | 否 | 覆盖 `~/.ostool/config.toml` 中的服务 URL |
| `port` | 否 | 覆盖服务 URL 中的端口，取值范围为 1 到 65535 |

`kernel_load_addr`、`fit_load_addr`、`bootm_addr` 和 `uboot_cmd` 必须与板卡的内存布局和服务端启动配置一致，不应从其他板型的 TOML 直接复制。`ostool 0.29.5` 使用 `shell_check_steps`，不再接受顶层的 `success_regex`、`shell_prefix` 和 `shell_init_cmd`。取得旧版板卡配置后，需要先把这些字段迁移到步骤列表。

每个 `shell_check_steps` 元素由 `ShellCheckStep` 解析，字段含义如下。

| 步骤字段 | 含义 |
| --- | --- |
| `shell_prefix` | 等待的 shell 提示符；后续步骤可省略并继承前一步的提示符 |
| `shell_cmd` | 观察到提示符后发送的命令；省略时只等待输出匹配 |
| `success_regex` | 任意一条正则表达式匹配时，该步骤完成 |
| `fail_regex` | 判定当前步骤失败的正则表达式列表；设置时必须同时指定 `success_regex` |
| `timeout` | 命令发送完成后的步骤超时，单位为秒；没有命令的步骤使用顶层超时 |

上面的完整板卡配置已使用步骤列表。迁移旧配置时，把顶层 `shell_prefix` 移入步骤，把 `shell_init_cmd` 改为 `shell_cmd`，并把成功标记正则移入步骤的 `success_regex`。板型、加载地址和全局 `fail_regex` 等字段继续放在步骤列表之前。

### 5.3 申请板卡并运行

确认客户端已登录、账号有 Banana Pi 的有效租约，并且两份配置位于 `./tmp` 后，在仓库根目录执行：

```bash
ostool board run \
  --config tmp/starry-bananapi-prebuilt.toml \
  --board-config tmp/starry-bananapi-board.toml
```

这条命令中的每一部分都有独立职责。

| 命令或参数 | 含义 |
| --- | --- |
| `ostool` | 启动客户端 |
| `board` | 选择远程板卡功能组 |
| `run` | 准备内核产物，申请板卡并执行启动流程 |
| `--config tmp/starry-bananapi-prebuilt.toml` | 读取预构建配置，确定要上传的 ELF |
| `--board-config tmp/starry-bananapi-board.toml` | 读取 Banana Pi 的板卡类型、加载地址和结果判定规则 |

`BoardRunArgs` 先加载 `--config` 指定的构建配置，准备其中的 ELF；随后加载 `--board-config`，按 `board_type` 向服务端申请板卡。该命令没有再传 `--server`、`--port` 和 `--board-type`，因为服务地址来自 `~/.ostool/config.toml`，板卡类型来自 `starry-bananapi-board.toml`。申请成功后，客户端上传启动产物、执行板卡启动流程，然后进入串口终端并按配置检查输出。

如果命令在申请会话前失败，优先检查两份 TOML 的路径和 `elf_path`。如果报租赁或板卡类型错误，使用 `ostool board ls` 确认服务地址、凭据和 `board_type`。已进入 U-Boot 但内核未启动时，则检查板卡配置中的加载地址、启动命令和服务端文件传输网络。

### 5.4 通过任务工具启动

任务工具的 `ostool` 依赖升级到兼容本文配置的新版后，可用 `cargo xtask starry board` 一次完成构建和启动，板卡配置使用第 5.2 节的新版文件：

```bash
cargo xtask starry board \
  --config os/StarryOS/configs/board/bananapi-bpi-sm10.toml \
  --board-config tmp/starry-bananapi-board.toml
```

当前 `bananapi` 分支仍依赖 `ostool 0.27.2`，不支持新版 `shell_check_steps`；其 `uboot-shell 0.2.7` 还会把时间戳误识别为 U-Boot 提示符，导致命令执行成功后仍超时。安装独立客户端不会升级这份依赖，因此升级前使用第 5.1 至 5.3 节的“xtask 构建、独立 `ostool 0.29.5` 启动”流程；内核构建本身正常。

## 6. TGOSKits 命令边界

TGOSKits 同时提供顶层开发板管理命令和面向操作系统的开发板命令。三类入口最终都可能使用 `ostool` 的开发板服务能力，但参数、构建责任和终端行为并不相同。

### 6.1 入口职责

选择入口时先看当前任务是直接使用独立客户端、管理开发板，还是构建并运行某个操作系统。下表中的命令不可只按名称互换。

| 入口                           | 主要用途                                                                           | 是否负责构建和运行系统                        |
| ------------------------------ | ---------------------------------------------------------------------------------- | --------------------------------------------- |
| `ostool board ...`           | 独立客户端直接执行`config`、`ls`、`connect` 或 `run`；本文说明的是这条入口 | 只有`ostool board run` 按项目配置构建并运行 |
| `cargo xtask board ...`      | TGOSKits 顶层开发板管理，提供查询、配置和人工串口连接等仓库工作流                  | 不负责某个操作系统的完整构建和部署            |
| `cargo xtask <os> board ...` | 按 ArceOS、StarryOS 或 Axvisor 的任务工具流程构建、部署并运行系统                  | 负责对应操作系统的完整开发板运行流程          |

`cargo xtask board` 是仓库侧的薄封装，参数和附加能力可能不同于独立 `ostool` CLI。例如仓库命令可以带任务工具自己的会话文件参数，因此不要把本文的完整命令行直接复制到 `cargo xtask` 后面。

### 6.2 选择入口

只需要验证账号、租赁或独立客户端连接时，使用本文的 `ostool` 命令。进入 TGOSKits 工作区后，可以按任务目标选择仓库入口：

1. 查看板型、编辑服务器配置或进行人工串口调试时，使用 `cargo xtask board`。
2. 构建并在开发板上运行指定操作系统时，使用 `cargo xtask <os> board`，并按对应系统文档提供目标、分组或运行参数。
3. 排查两类仓库命令的参数和配置优先级时，查阅[板卡管理](../build/board.md)，不要根据独立 `ostool` 的选项猜测任务工具行为。

如果命令已经进入某个任务工具管理的流程，应根据当前活动终端判断退出方式。只有 `ostool` 串口终端启用了退出序列时才使用 `Ctrl+A` 后按 `x`；QEMU 和其他提示符应按对应文档或界面提示操作。
