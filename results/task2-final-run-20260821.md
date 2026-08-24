# Task 2 最终交付运行记录

## 当前结论

Task 2 的协议、双向 UDP/IP 数据面、可靠性状态机和隔离验证已经完成 QEMU
AArch64 SIL 验收。本文只记录可提交的结论，不把旧的失败运行伪装成成功。

## 构建与检查

```bash
scripts/test/net-dual-guest/build-linux-task2.sh
scripts/test/net-dual-guest/build-linux-initramfs.sh
TASK2_ZEPHYR_VIRTIO_SLOT=0 scripts/test/net-dual-guest/build-zephyr-task2.sh
cargo test -p task2-net-protocol
python3 -m unittest discover -s scripts/test/net-dual-guest -p 'test_*.py'
```

当前 HEAD 回归结果：`task2-net-protocol` 21/21 通过；网络验证器测试通过。

## 运行拓扑

```text
Linux vCPU0 / VM[1] 10.0.42.15:4242
        │ VirtIO-net / AxVisor internal L2 switch
Zephyr vCPU0 / VM[2] 10.0.42.2:4242
```

两侧使用独立 VM stage-2、DMA carveout、VirtIO 队列和 vIRQ route。QMP 不承载
应用数据，只用于 `drop`、恢复和退出。

## 已有运行证据

| 证据 | 结果 |
|---|---|
| 双向 T2N1 pcap ledger | Linux/RTOS 两侧方向、类型、序号和 ACK 对账一致 |
| CONTROL/STATUS | 正常请求、状态回传和 ACK 完成 |
| 可靠性 | ACK 丢失后重传，重复帧不重复交付，乱序/非法参数产生 ERROR |
| 链路故障 | blackout 后进入 Safe，恢复后重新同步并继续控制环 |
| 长稳 | 历史约 1 小时 QEMU 运行无非预期 Safe、协议错误或发送错误 |
| 隔离 | FDT endpoint、stage-2 MMIO、GPA/HPA、DMA 和 SPI route 检查通过 |

原始 pcap、run.log、manifest 和故障证据分别归档于 `results/task3/` 与
`results/task3/switch/`；协议和命令说明见
`book/design/task2-dual-guest-network-final.md` 及
`scripts/test/net-dual-guest/README.md`。

## 尚未冒充完成的部分

- 完整双 Guest QEMU 流程仍由显式脚本驱动并保存成功/失败退出码；协议和
  controller contract gate 已接入仓库默认 CI。
- session-mismatch Heartbeat 的 ERROR 语义已补 Rust/Python 回归，并在 Zephyr
  parser 中区分“格式错误”和“陌生 session”；陌生 session 会返回
  `ERROR(SessionMismatch, acknowledgement=0)`，不改变 Safe 状态。
- QEMU 结果不等同于物理板网络吞吐或硬实时上界。
