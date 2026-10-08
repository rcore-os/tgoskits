# qemu-http-control-plane

在 QEMU aarch64 上以真实 TCP 驱动 AxVisor 的管理 HTTP 控制平面，验证 `/api/vms`
生命周期契约。

## 怎么跑

```bash
cargo xtask image pull
cargo xtask axvisor test qemu --arch aarch64 --test-case http-control-plane
```

`http-control-plane/qemu-aarch64.toml` 的 `[host_http_probe]` 让 runner 追加
hostfwd 与 virtio-net-pci 网卡，并在转发端口可达后执行
`http-control-plane/http_probe.py`；脚本退出码即用例判定。`fail_regex` 仍会在
guest 内核 panic 时直接失败。

## 用例做了什么

默认 VM（id=1，`http-control-plane/vm-memory.toml`，构建期内嵌 `memory` 镜像）
被 `no-auto-start` 保持在 `Ready`。探针在一轮启动内驱动完整契约：list/detail、
鉴权与错误映射（401/400/404/409）、启动、暂停/恢复循环、停止、从 `Stopped`
重新启动、销毁后按同一内嵌镜像重建并再次运行，最后清理。

判定不依赖脚本里的固定睡眠，而是轮询 `GET /api/vms/{id}` 上的两个 VM 级观测：

- `guest_entry_count` 只在 guest 真正（重）进入后自增，用于证明 resume（或
  重新 start）确实唤醒了 vCPU 并重新进入 guest，而不是仅翻转了状态。
- `guest_park_count` 只在 vCPU 真正观察到暂停态并 park 时自增，用于证明 pause
  确实落到 vCPU 上，并确认重复 pause 不会产生新的 park。

固定的 `phys_cpu_ids = [1]` 让 vCPU 落在一个非主核的固定 CPU 上，由 0 号核上
的管理通道发出唤醒——这正是最容易暴露唤醒路径缺陷的配置。

## 生命周期口径

- `create`、`start`、`resume`、`delete` 等待操作**完成**再响应。
- `pause`、`stop` 在操作被**接受**后立即响应（`"async": true`）；`Paused`/
  `Stopped` 快照只在参与者与设备/端口真正安静（pause）或整轮拆除完成
  （stop）之后才出现，探针轮询终态而非相信“已接受”。
- 重复 `pause`（`Paused`）、重复 `resume`（`Running`）、重复 `stop`
  （`Stopped`）都是幂等的 200；`start` **不是**幂等的，对 `Running` 再次
  `start` 仍返回 409。
- 从 `Stopped` 重新 `start` 是受支持的状态转换：用例要求真实 200、真实进入
  guest（新 run 的 `guest_entry_count` 重新从 0 起并严格推进），然后才允许
  再次停止。旧的“重启后必须 409”的已知限制已删除。
- 停止后 VM 无 run 记录，两个计数器发布为 0；因此“停止 → 再次启动”读到的
  是新 run 从 0 开始的新鲜计数。

详细契约、字段与错误码见
`os/axvisor/doc/http-control-plane-quickstart.md`。

## 未覆盖的真实限制

- 计数器是 VM 级聚合，只证明“至少一个 vCPU 取得进展”，不是逐 vCPU、设备或
  定时器的静默证明。
- 本用例从 HTTP 观察状态与 guest 进展；定时器回调、设备 worker 和 DMA 的
  逐参与者静默仍需对应的运行用例验证。
