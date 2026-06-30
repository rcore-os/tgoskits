# ISSUE-002: Apache smoke 在 reviewer 环境失败与 TCP_DEFER_ACCEPT 探查

## 状态

- 状态：本地无法复现 reviewer 的失败；新增定向 debug 测试以定位根因
- 范围：StarryOS x86_64 默认 app workflow `apache smoke`
- 结论：`TCP_DEFER_ACCEPT` 的 `setsockopt` 在旧内核返回 `ENOPROTOOPT`（errno 92）属实，
  但定向实验证明该错误只是一条警告，不会使监听 socket 不可用，
  因此它不足以解释 reviewer 看到的 readiness curl 超时。真正根因仍需 reviewer 侧信息确认。

## 现象（review 反馈）

reviewer 运行默认命令：

```bash
timeout 1800s cargo xtask starry app qemu -t apache --arch x86_64
```

guest 内 `/usr/bin/apache-runner.sh smoke` 在 `start apache single process` 阶段失败：

```text
APACHE_RUNNER_PHASE_BEGIN phase=smoke
APACHE_APP_STEP_PASS: prepare packages
APACHE_APP_STEP_PASS: prepare apache files
APACHE_APP_STEP_PASS: environment probe
APACHE_APP_STEP_PASS: apache config test
APACHE_APP_STEP_FAIL: start apache single process
APACHE_APP_SMOKE_FAILED failures=1 status=1
APACHE_RUNNER_PHASE_FAIL phase=smoke rc=1
APACHE_RUNNER_FAILED
```

反馈描述：诊断日志里 httpd 进程有 pid，error/stdout 中可见的唯一异常是

```text
(92)Protocol not available: AH00076: Failed to enable APR_TCP_DEFER_ACCEPT
```

而 readiness 的 curl 在 30 秒内一直没有成功，于是外层 `fail_regex` 命中
`APACHE_RUNNER_FAILED`，xtask 返回失败。

## 失败链路定位

1. `start apache single process`（`apps/starry/apache/smoke/apache-smoke-tests.sh` 的
   `start_httpd`）是后续所有 HTTP 节点的前置步骤；它失败导致整轮 smoke 失败。
2. `start_httpd` 返回失败的唯一条件，是它在 30 秒内对本地监听端口 `127.0.0.1:8080`
   反复发起的 readiness curl 始终没有成功。
3. reviewer 日志中与该阶段同时出现的唯一异常是 `AH00076 / errno 92`。它来自
   Apache/APR 建立监听 socket 时（`make_sock`）按 `AcceptFilter` 对 listen fd 调用
   `setsockopt(IPPROTO_TCP, TCP_DEFER_ACCEPT, ...)`。
4. 旧内核把 `(PROTO_TCP, TCP_DEFER_ACCEPT)` 标为未实现
   （`os/StarryOS/kernel/src/syscall/net/opt.rs` 的 TODO 列表），`sys_setsockopt`
   落到默认分支返回 `ENOPROTOOPT`（errno 92），与日志中的 `(92)Protocol not available`
   吻合。

到第 4 步为止只能确认“errno 92 来自这条 setsockopt”，但不能确认它就是
readiness 失败的根因——Apache 把 `AH00076` 作为警告记录后会继续监听，
监听 socket 是否仍然可用需要实测。

## 定向 debug 测试

为了在不依赖具体 apache2/APR 构建的前提下隔离内核行为，新增一个静态编译的
setsockopt 探针，并用一个 debug 专用 qemu 配置驱动：

- `apps/starry/apache/debug/tcp-defer-accept-probe.c`：在 TCP listen socket 上
  按 Apache/APR 的顺序执行 `bind -> listen -> setsockopt(TCP_DEFER_ACCEPT)`。
  关键是 **setsockopt 失败后不中止**，完全复刻 Apache 记录 `AH00076` 警告后继续运行的行为；
  随后由子进程对 `127.0.0.1` 发起真实 `connect`，父进程 `accept` 并读取数据，
  以此判断监听 socket 在 setsockopt 失败后是否仍能正常接受连接。
- `apps/starry/apache/debug/apache-tcp-defer-accept-probe.sh`：把探针结果映射到
  runner 的 PASS/FAIL 标记。
- `apps/starry/apache/qemu/debug/qemu-x86_64-tcp-defer-accept-probe.toml`：debug 专用
  qemu 配置，通过 `--qemu-config` 选择运行，不影响默认 smoke / phase workflow。
- `apps/starry/apache/prebuild.sh`：仅当显式提供探针二进制路径
  （环境变量 `APACHE_DEBUG_PROBE_BIN`）时才注入该二进制，默认 workflow 不受影响。

运行方式：

```bash
cargo xtask starry app qemu -t apache --arch x86_64 \
  --qemu-config qemu/debug/qemu-x86_64-tcp-defer-accept-probe.toml
```

## 实验结果

两次运行的唯一变量是内核侧的 `TCP_DEFER_ACCEPT` 处理，rootfs、apache 资源、网络栈一致。

未实现 `TCP_DEFER_ACCEPT` 的内核：

```text
TCP_DEFER_ACCEPT_SET_FAIL rc=-1 errno=92 (Protocol not available) (continuing, like Apache AH00076 warning)
CLIENT_CONNECT_OK
SERVER_ACCEPT_OK got=4 bytes payload="PING"
VERDICT: setsockopt(TCP_DEFER_ACCEPT) FAILED but connect/accept STILL WORKS -> AH00076 is only a warning; errno 92 is NOT the curl-timeout root cause
PROBE_RESULT_WARNING_ONLY
```

实现 `TCP_DEFER_ACCEPT`（store-and-ignore）的内核：

```text
TCP_DEFER_ACCEPT_SET_OK rc=0
CLIENT_CONNECT_OK
SERVER_ACCEPT_OK got=4 bytes payload="PING"
VERDICT: setsockopt(TCP_DEFER_ACCEPT) OK and connect/accept works -> fixed kernel behaves correctly
PROBE_RESULT_FIXED_OK
```

| 内核 | setsockopt(TCP_DEFER_ACCEPT) | 之后 connect / accept | 判定 |
| --- | --- | --- | --- |
| 未实现 | `rc=-1 errno=92` | 成功，收到 `PING` | WARNING_ONLY |
| 实现 | `rc=0` | 成功，收到 `PING` | FIXED_OK |

## 结论

- `errno 92` 来自 `TCP_DEFER_ACCEPT` 的 setsockopt，这一点与 reviewer 日志一致。
- 但实验证明：即使 setsockopt 返回 errno 92，监听 socket 仍可正常 `accept` 连接。
  因此 `AH00076` 在这里是一条警告，**不足以解释 readiness curl 的 30 秒超时**。
- 本地多次复测（含清理 rootfs 镜像与 app 构建缓存重建）均无法复现 reviewer 的失败：
  本地从 Alpine 镜像拉到的 apache2 构建在 smoke 中不会触发该 setsockopt 失败，
  smoke 全程通过。
- 推断（未经 reviewer 侧数据证实）：reviewer 环境与本地拉到的 apache2/APR 构建可能不同，
  导致是否走 `TCP_DEFER_ACCEPT` 路径有差异；但即便走了该路径，按上述实验它也只产生警告，
  readiness 超时的真正根因可能另在网络时序、地址获取或环境相关因素，需要更多日志确认。

## 待 reviewer 提供（任一或多项）

为闭合根因分析，请 reviewer 提供以下信息中的一项或多项：

1. 复现该失败的运行环境说明（host OS / 架构、qemu 版本、网络模式）。
2. 失败那次实际拉到的 apache2 / apr 包版本，以及 `httpd -v` 与 `httpd -V` 完整输出。
3. 失败那次 guest 内的完整诊断：`error.log`、httpd stdout 日志、readiness 阶段
   curl 的具体报错（是 connection refused 还是 timeout），以及当时的监听 socket
   状态（如 `ss -ltnp` 或等价输出）。
4. 若可行，在相同环境下运行上述 debug 探针配置并回贴 `VERDICT` 行，
   以确认监听 socket 在该环境下是否真的不可用。

## 备注

- 内核侧已准备 `TCP_DEFER_ACCEPT` 的 store-and-ignore 实现（保存设置值、返回成功，
  accept 路径不实际延迟），可消除 `AH00076` 警告并与 Linux 行为对齐；
  该内核改动暂不随本 debug 测试一起提交，待根因确认后再决定。
