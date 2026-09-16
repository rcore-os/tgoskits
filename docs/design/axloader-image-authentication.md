# Axloader 镜像真实性校验

## 1. 问题与信任边界

### 1.1 当前缺口

议题 [#1749](https://github.com/rcore-os/tgoskits/issues/1749) 中的串口启动入口已由 #2354 移除，但真实性缺口仍然存在。`control::fetch_boot_offer` 从 UDP 发现的 HTTP 服务取得 `kernel_sha256`；`elf_loader::download_and_load` 只证明下载内容与该摘要一致。能够替换控制响应的攻击者可以同时替换 ELF 和摘要。

`integrity::sha256_matches` 应继续负责传输完整性。来源真实性需要由网络响应之外预先配置的信任依据建立；这与 [RFC 9019 第 6 节](https://www.rfc-editor.org/rfc/rfc9019.html#section-6) 对固件清单与信任锚的区分一致。

### 1.2 目标与限制

装载器必须在解析 ELF、分配装载区、复制 segment 和交接控制流之前验证发布者授权。缺少信任依据、未签名镜像、字节篡改、错误签名和入口选择篡改均必须拒绝；合法授权镜像仍应通过真实 UEFI HTTP 路径装载。

EFI 文件及其中的公钥由部署管理员负责保护。镜像签名不提供网络保密性、服务可用性、设备身份认证或旧版本撤销；允许重新启动已经授权的镜像。若需要抗回滚，还需独立设计可信持久版本状态，不能把 `boot_id` 或 MAC 当作密码学依据。

## 2. 方案比较

### 2.1 可选边界

选择方案需要同时考虑攻击模型和现有频繁换内核的实验室流程，不能只让下载摘要测试通过。

| 方案 | 保证与代价 |
| --- | --- |
| 仅说明网络隔离要求 | 明确部署假设，但不能达到议题的真实性验收条件 |
| 固定内核摘要 | 可复用 SHA-256，改动最少；每次内核更新都需要重新构建并安装 EFI |
| 固定公钥、签名启动清单 | 保留动态换内核，能绑定会话与控制字段；需要配套修改 `httpboot-protocol`、服务端签名及发布流程 |
| 固定公钥、签名镜像 | 服务端可继续传输文件；签名必须覆盖镜像和入口模式，并在所有 ELF 后处理结束后生成 |
| HTTPS 与预配置服务身份 | 需要固件证书验证、可信服务名及部署证书管理；只允许 HTTPS scheme 或相信发现响应中的主机名不足以授权内核 |

现有 #2354 与 `drivercraft/ostool#179` 明确保留了 #1749，未提供可复用的真实性实现。应在现有下载装载边界增加验证，而不建立另一套网络发现或设备绑定机制。

### 2.2 签名镜像格式

采用签名镜像方案，保留 ELF 头与 segment 布局，在文件尾部附加 81 字节签名记录：16 字节 `AXLOADER-SIG-V1\0`、1 字节入口模式、64 字节 Ed25519 签名。模式 0 表示默认 ELF 入口，模式 1 表示 `httpboot_entry`。`authentication::sign_image` 签名覆盖完整原始 ELF、格式标记和入口模式；`authenticate_image` 使用 `ed25519-dalek 2.2` 的 `verify_strict` 校验后，只把原始 ELF 切片交给 `load_elf`。控制响应中的 `entry_symbol` 必须与签名入口模式一致，避免由未认证控制字段选择交接方式。

`ostool::artifact::runtime` 在启用 ELF 后处理时会通过 `llvm-objcopy` 重写 ELF，因此不能在该步骤之前追加签名。签名处于所有 strip、符号处理及格式转换之后、上传之前。`kernel_size` 与 `kernel_sha256` 针对最终上传文件计算。现有 ostool HTTP 上传和服务端存储都保留完整文件，故无需扩展 `httpboot-protocol`。

`AppContext` 的板卡入口在显式配置 `AXLOADER_SIGNING_KEY` 时，通过 `sign_runtime_kernel` 签名最终 ELF 到独立临时目录，然后以不重写 ELF 的 `RuntimeArtifactInput` 注册产物。`SignedKernel` 所有者存活到板卡运行结束；原 Cargo 产物不被修改。ostool HTTP 上传固定使用 `httpboot_entry`，自动签名绑定同一选择。远端启动模式由 ostool 内部取得，调用方不猜测 `board_type`，该环境变量只应在 HTTP Boot 命令作用域设置。

## 3. 实现与验证约束

### 3.1 失败与迁移

真实性校验失败应沿现有 `ElfLoadError` 和 `LoaderStatusPhase::Failed` 返回，保留失败 `boot_id` 不重试的策略；网络对象仍按现有所有权释放。缺少公钥不得退回仅校验网络摘要。私钥仅属于发布端，不进入 EFI、仓库、日志或测试产物中的部署配置。

启用强制验签会拒绝现有未签名上传，迁移必须同时准备签名发布入口、部署公钥并更新 EFI。回滚到旧 EFI 会重新开放真实性缺口，不能作为校验失败时的自动恢复。新格式不能被旧装载器识别为已验证镜像。

### 3.2 验证与审查

回归应覆盖真实生产校验逻辑，先在旧实现上证明未认证 ELF 被接受，再在修复后证明拒绝。组件层验证签名与入口绑定；真实 QEMU 层证明拒绝发生在 ELF 装载之前，同时验证正常授权镜像的 GET、状态报告和装载成功。

本设计涉及启动安全边界，需要领域审查后再合入；本地验证不替代该审查。下载缓冲区仍由既有 HTTP 路径拥有，验签借用同一缓冲区，不分配第二份内核。签名增加固定 81 字节传输开销，以及 Ed25519 校验所需的散列和曲线运算；现有 SHA-256 传输校验保留。

### 3.3 回归证据

在尚未接入 `authenticate_image` 的真实 `download_and_load` 上，`cargo xtask axloader test qemu --target x86_64-unknown-uefi --accel tcg` 的未签名用例观察到 `elf_loaded:`，以 `unauthenticated kernel reached ELF load or handoff` 失败。接入校验后，同一入口的结果如下；每个用例的 HTTP 摘要都由实际传输内容计算，因此篡改用例不会被原 SHA-256 检查提前拦截。

| 输入 | 可观察结果 |
| --- | --- |
| 未签名合法 ELF | `Authentication(MissingSignature)`，没有 ELF 装载成功或交接状态 |
| 修改已签名 ELF 的负载指令，同时更新 HTTP 摘要 | `Authentication(InvalidSignature)`，没有 ELF 装载成功或交接状态 |
| 修改签名字节，同时更新 HTTP 摘要 | `Authentication(InvalidSignature)`，没有 ELF 装载成功或交接状态 |
| 可信私钥签名的 ELF | 内核 GET、`ready_to_handoff` 和 `elf_loaded:` 均成功 |

同一任务入口运行的 14 个 axloader 宿主测试覆盖缺少公钥、弱公钥、其他私钥签名、入口模式不匹配及完整文件逐字节篡改。QEMU 环境为 x86_64、固定版本 OVMF、q35 和 TCG；没有使用实体板卡，也没有更新任何部署密钥或 EFI。安装仍需由部署管理员完成，不能以本地 QEMU 结果表示板卡已迁移。
