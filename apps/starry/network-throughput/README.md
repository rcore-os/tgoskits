# StarryOS 网络吞吐基准

`network-throughput` 在 Orange Pi 5 Plus 或 AKA-00-SG2002 上测量到 ostool-server 的 HTTP 流式吞吐。它使用专用 TCP 3000 端口，不依赖 iperf 客户端或服务端；管理 API 继续使用 2999。

## 1. 传输模型

`network-bench.sh` 为每条流调用 `POST /v1/tests` 创建独立测试。双向场景在同一个 ID 上并行运行上传和下载；多流场景为各流创建不同 ID，因而能同时检查板端和服务器的并发路径。

### 1.1 流量生成

T01–T07 依次覆盖单流 TX、单流 RX、单流双向、2/4/8 流 TX 和 4 流 RX。TX 表示板端上传到 `PUT /v1/tests/{id}/upload`，RX 表示板端从 `GET /v1/tests/{id}/download?duration_secs=N` 下载。`c/CMakeLists.txt` 交叉编译共享的 `upload-source.c`：它按单调时钟连续写出 128 KiB 固定块，不缓存完整传输；下载响应由 curl 直接丢弃。

### 1.2 结果判定

传输结束后，`network-bench.sh` 查询 `GET /v1/tests/{id}` 核对终态。每条流必须为 `completed` 且有实际字节数。上传的服务端字节数必须等于 `upload-source` 报告的有效载荷字节数，下载的服务端字节数必须等于 curl 的 `size_download`。每流平均速率按服务端最终字节数和单调时钟耗时计算；并行流速率相加。多流上传时状态查询可能被数据流延后，因此不在传输中查询预热快照。

## 2. 板卡运行

`init.sh` 只从本次会话下载并执行 `bootstrap.sh`，避免把完整下载逻辑塞进串口命令。后者下载 `network-bench.sh` 与静态链接的 `upload-source`；若板端没有 curl，则先用 wget 下载本次会话打包的 curl 和 musl 依赖。板卡配置通过 `${boardServerIp}` 提供当前服务端地址，因此不需要固定板卡 IP。两个板卡配置都存在，运行时必须显式选择 `--board-config`。

### 2.1 启动命令

OrangePi 可以直接运行；AKA 构建还需要 Wi-Fi 凭据，以便启动后取得地址。两者均由 `cargo xtask` 负责构建、会话文件上传、串口运行和失败标记检查。

```bash
cargo xtask starry app board -t network-throughput -b OrangePi-5-Plus \
  --board-config board-orangepi-5-plus.toml

STARRY_WIFI_SSID='<ssid>' STARRY_WIFI_PASSWORD='<password>' \
  cargo xtask starry app board -t network-throughput -b AKA-00-SG2002 \
  --board-config board-aka-00-sg2002.toml
```

### 2.2 测量档位

默认每场景运行三轮，每轮 10 秒，每轮后冷却 15 秒；打印三个样本及中位数，不设与机器绑定的吞吐门槛。终态快照、逐流速率和汇总保存在 `${TMPDIR:-/tmp}/starry-network-bench/`。可通过 `STARRY_NETWORK_BENCH_DURATION` 和 `STARRY_NETWORK_BENCH_COOLDOWN` 临时缩短诊断运行；正式结果使用默认值。

本应用测量真实网络链路的 HTTP 流式吞吐。[LTP netstress 手动应用](../qemu/ltp-netstress/README.md)测量请求响应和短连接性能，两个指标不能互换。
