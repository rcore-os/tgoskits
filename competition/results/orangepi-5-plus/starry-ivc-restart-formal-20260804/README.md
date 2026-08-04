# StarryOS guest-restart formal campaign

结果：**PASS（3/3）**。三次运行均在 Orange Pi 5 Plus（RK3588）上实际停止并重建 VM 1，StarryOS 与 Zephyr 在新 session 恢复控制，严格分析器、逐文件校验、结果镜像 fsck、Linux 恢复和最终根分区读写检查全部通过。

## 证据身份

- 正式采集源码：`6adf49e09ce91b53d2573cb8d34c60dc6a9ec47c`，clean detached worktree。
- 正式采集时间：2026-08-04 00:10:31Z 至 00:22:00Z。
- 板卡：`OrangePi-5-Plus`，证据 board ID `bf61f4d4a1d994ad`。
- restart rootfs SHA-256：`9e092ad3e0ec4c9842732f8ff0b9475005f1fe2c80cf35d329d9e77b6e7e9ca4`。
- campaign summary SHA-256：`935db25de96c83267b8d11ea8e55a2909a42a07ca8f2614cef687dce153e2302`。
- preregistration SHA-256：`6ea0dcb7774f30400ab606d309212f8e72a28ee89e544bcdb9b25a0e39df4530`。
- post-capture amendment SHA-256：`8336e788261c66116f38556a2720bff544fa6cbbb0c8fab530a4185c2cf46701`。
- final Linux check SHA-256：`fbecc79e63a25e1791e8fe71ae0295fd890f3fd4665599b8cfa9e09a703fc584`。

`campaign-preregistration.json` 是对 `execution-plan-pre-capture.md` 中预先冻结契约的机器可读转录。冻结计划的 mtime 为 00:09:22Z、SHA-256 为 `bffedc3c6a9d478eeb25f43a8b0dac84e7505ebd7017411afaf646049c6972d1`，早于首轮采集 69 秒；JSON 在采集后为聚合器生成，并在文件内明确记录该事实。

## 严格契约与结果

每轮固定满足：

- 20 个 reset 前 fresh command，加 100 个 reset 后 fresh command；accepted/applied 均为 120。
- 显式重复新 session 的 `seq=1` 一次；duplicate 为 1，STATUS/ACK 均为 122。
- ERROR/protocol error 均为 1。
- session reset、session rejection、safe fallback、endpoint recovery、stale STATUS、stale ACK、retired CONTROL rejection 均为 1。
- VM 1 在 host CPU 3 上真实 reset；请求和观测 delay 均为 20,000 ms，ready wait 均为 10 ms。
- 每轮 result image 的 ext4 fsck 为 clean，并恢复 Linux。

| run | pre-reset deadline miss | pre-reset p99 / max | post-reset deadline miss | post-reset p99 / max | 结果 |
| --- | ---: | ---: | ---: | ---: | --- |
| 001 | 1 | 5,455 / 126,624 µs | 0 | 8,885 / 25,014 µs | PASS |
| 002 | 1 | 11,528 / 119,855 µs | 0 | 8,402 / 22,390 µs | PASS |
| 003 | 1 | 8,370 / 124,313 µs | 0 | 8,366 / 24,609 µs | PASS |

三次 pre-reset 各出现一次 deadline miss，是保留的不利描述性结果；它没有被隐藏，也没有被用于放宽 restart 协议门。post-reset 三轮 deadline miss 均为 0。该 campaign 证明重启恢复，不承担实时隔离改善结论。

## 完整性处理

CH340 在三次 pre-reset 记录中保留了与完整 digest 一致的 12 位 SHA 前缀。snapshot manifest、harvest record 和实际 raw bytes 分别给出并一致确认完整 SHA-256。聚合器修复提交 `7c1ba13af577d82fe89b912bde868604763618f0` 只允许合法前缀匹配独立完整 digest，同时继续拒绝分叉片段；修复采用 regression-first，完整 IVC Python 测试为 112/112。

每个 `run-*` 目录均保留 `console.log`、两阶段 raw CSV、gzip twin、metadata、summary 与 `checksums.sha256`。聚合器再次验证了所有 manifest、gzip/plain 字节一致性、clean source、rootfs hash、同一 board ID、精确协议计数和生命周期门。

最终健康检查在独占 board lease 内完成：`/dev/mmcblk1p2` 为 `ext4,rw`，写入、sync、读回、删除和再次 sync 均成功；释放 lease 后 board pool 为 1/1 available。

## 证据边界

`starry-ivc-restart-smoke-*` 与 `starry-ivc-restart-debug-*` 目录全部排除在本 campaign 之外。失败 smoke 保持原失败结论，离线 replay 仅证明修复，不会被重标为正式证据。本目录的正式运行仅为 `capture-001/fault-restart/run-001..003`。

## 复核命令

```bash
python3 competition/ivc/aggregate_board_campaign.py \
  competition/results/orangepi-5-plus/starry-ivc-restart-formal-20260804 \
  --result-root capture-001/fault-restart \
  --latest-amendment campaign-amendment-001-post-capture-aggregation.json \
  --final-board-check final-board-linux-root-check.json \
  --output /tmp/restart-campaign-summary.json
```

输出的 `assessment.campaign_gate_met`、`all_manifests_verified`、`all_raw_and_gzip_twins_verified`、`exact_fault_contract_met` 与 `final_board_linux_root_rw` 必须全部为 `true`。
