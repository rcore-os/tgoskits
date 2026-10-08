# AxVisor 管理 HTTP 控制平面快速上手

AxVisor 内置一个基于 axum 的管理控制平面：它在虚拟机监控器内部监听 TCP，
通过 JSON over HTTP 暴露 VM 生命周期操作与只读观测。宿主脚本或 CI 可以借此
创建、启动、暂停、恢复、停止和销毁 VM，而不必进入串口控制台。

- 监听地址来自构建期环境变量 `AXVM_HTTP_BIND`，缺省为 `127.0.0.1:8080`。
- 修改状态的请求需要 `Authorization: Bearer <token>`；token 来自构建期
  `[env] AXVM_HTTP_TOKEN`。只读 GET 无需鉴权。
- 常与 QEMU user-mode 网络配合：构建配置追加 hostfwd，宿主经转发端口访问
  guest 内的服务器。

## 构建与运行

CI 与本地共用同一入口（用例位于 `test-suit/axvisor/normal/qemu-http-control-plane/`）：

```bash
# 预取 QEMU 镜像与托管 rootfs（用例的 BusyBox initramfs 由它生成）
cargo xtask image pull

# 运行 http-control-plane 用例（aarch64，默认 test-group = normal）
cargo xtask axvisor test qemu --arch aarch64 --test-case http-control-plane
```

用例构建配置
`test-suit/axvisor/normal/qemu-http-control-plane/build-aarch64-unknown-none-softfloat.toml`
打开 `http-axum` 与 `no-auto-start`，把默认 VM 留在 `Ready`，并设置：

```toml
[env]
AXVM_HTTP_TOKEN = "axvisor-http-test-token"
AXVM_HTTP_BIND = "0.0.0.0:8080"
```

`qemu-aarch64.toml` 中的 `[host_http_probe]` 让 runner 追加 hostfwd 与
virtio-net-pci，并在转发端口可达后执行 `http-control-plane/http_probe.py`；
该脚本的退出码即用例判定（0 通过，非 0 失败），QEMU 串口输出不是判定来源。

## 路由与返回

| 方法 | 路径 | 语义 | 成功码 |
| --- | --- | --- | --- |
| GET | `/api/vms` | 列出所有 VM 摘要 | 200 |
| GET | `/api/vms/{id}` | 单个 VM 详情 | 200 |
| POST | `/api/vms/create` | 从 TOML 创建并注册 | 200 `{"id": n}` |
| DELETE | `/api/vms/{id}` | 销毁并注销 | 204 |
| POST | `/api/vms/{id}/start` | 启动（等待完成） | 200 |
| POST | `/api/vms/{id}/pause` | 请求暂停（接受即返回） | 200 |
| POST | `/api/vms/{id}/resume` | 恢复（等待完成） | 200 |
| POST | `/api/vms/{id}/stop` | 请求停止（接受即返回） | 200 |

详情响应字段：`id`、`name`、`status`、`cpu_num`、`memory_mb`、
`vcpu_states`、`guest_entry_count`、`guest_park_count`。列表（摘要）不含
vCPU 与计数器字段。

动作响应统一为 `{"ok": true, "status": "<当前状态>", "async": <bool>}`。

### 已接受（accepted）与已完成（completion）

- `create`、`start`、`resume`、`delete`：handler **等待操作完成**再响应。
  创建完成表示资源已准备；启动／恢复完成表示 vCPU owner 已初始化／恢复、
  准入已打开且已唤醒；销毁完成表示资源与实例已释放。首个真实 guest 执行进展
  另行轮询 `guest_entry_count`，不由启动／恢复的 200 同步保证。
- `pause`、`stop`：handler 在操作被**接受**后立即响应（`"async": true`）。
  `Paused`/`Stopped` 状态只有在 owner 真正让所有参与者 park、让设备/端口
  安静（pause），或完成整轮拆除（stop）之后才出现。调用方应轮询详情取得
  终态，不要把“已接受”的响应当作“已完成”。

### 幂等与状态转换

- `pause` 在 `Paused` 上重复调用返回 200，且不会产生重复 park。
- `resume` 在 `Running` 上重复调用返回 200（幂等空操作）。
- `stop` 在 `Stopped` 上重复调用返回 200，不会复活任何运行资源。
- `start` **不是**幂等操作：对已经在 `Running` 的 VM 再次 `start` 返回 409。
- `Ready`（尚未启动）拒绝 `pause`/`resume`（409）。
- 从 `Stopped` 重新 `start` 是受支持的状态转换：返回 200，且新 run 必须
  真正进入 guest（见下方的运行计数器）。

### 错误映射

| 状态码 | 含义 |
| --- | --- |
| 400 | 请求体非法：缺少 `toml` 字段，或 TOML 无法解析 |
| 401 | 修改状态请求缺少或携带错误的 Bearer token |
| 404 | `{id}` 非数字或未知 VM |
| 409 | 非法状态转换、重复注册、陈旧 run、入口已关闭等冲突 |
| 503 | 宿主资源暂不可用（内存、vCPU、设备，或操作被取消） |

## 运行计数器

`guest_entry_count` 与 `guest_park_count` 是 **按 run** 的 VM 级聚合计数，
由 vCPU owner 维护。`VmHandle::snapshot` 从 owner 发布的本运行观察集合读取
实时原子计数，不要求再发生生命周期事件：

- `guest_entry_count` 只在一次真实 guest 执行返回 VM exit 后递增；失败 wake
  或 `EngineOutcome::Interrupted` 重试不会推进它。
- `guest_park_count` 在 vCPU 真正观察到暂停态并 park 时自增。

两者只能证明“至少有一个 vCPU 取得了进展”，不是逐 vCPU 的静默保证。VM 处于
`Stopped` 时（无 run 记录）两者发布为 0，下一次 run 从 0 重新开始。

## 限制与注意

- `pause`/`stop` 是请求语义：终态快照相对响应是滞后的，取决于各参与者的
  拆除进度。
- 计数器是 VM 级聚合，不能证明每个 vCPU、设备或定时器都已静默。
- `Paused` 在 vCPU 卸载、任务定时器 producer 与设备后台执行静默后发布。
  已发布的 pending 与逻辑定时器截止时间保留，恢复后继续消费；宿主单调时间照常前进。
- 直通设备缺少 DMA 静默能力时，相关资源更新与回收返回错误并保留 backing。
- `create` 只能实现构建期内嵌镜像（`image_location = "memory"`）且
  `base.id` 匹配的配置；镜像未内嵌时创建失败。
- 观测通过轮询 `GET /api/vms/{id}` 完成；不要用固定睡眠代替事件轮询。

## 参见

- 用例与探针：`test-suit/axvisor/normal/qemu-http-control-plane/`
- HTTP handler：`os/axvisor/src/http/vm.rs`、`os/axvisor/src/http/server.rs`、
  `os/axvisor/src/http/auth.rs`
- owner 生命周期：`virtualization/axvm/src/control/`
