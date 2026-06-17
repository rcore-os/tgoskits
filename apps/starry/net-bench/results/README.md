# net-bench baseline results

本目录用于保存 StarryOS 网络基线测试结果。

## 文件约定

- `baseline-*.txt`：人工整理后的基线汇总，可提交，用于记录环境、命令和结果摘要。
- `starry-*.txt`：`apps/starry/net-bench/run.sh` 生成的单次运行日志，默认不提交。
- `iperf3-server-*.log`：host 侧 iperf3 server 日志，默认不提交。

## 推荐流程

```sh
# 默认 SLIRP/smp=1
bash apps/starry/net-bench/run.sh aarch64 slirp

# SLIRP/smp=4
bash apps/starry/net-bench/run.sh aarch64 slirp-smp4

# TAP/smp=1，需提前配置 tap0
bash apps/starry/net-bench/run.sh aarch64 tap
```

若 host 已配置好 `tap0=192.168.100.1/24`，也可以运行：

```sh
bash apps/starry/net-bench/run.sh aarch64 all
```

## 汇总模板

~~~md
# StarryOS 网络基线汇总 - YYYY-MM-DD

## 环境

- Host OS:
- QEMU:
- iperf3:
- Arch:
- Rootfs:
- Commit:

## 命令

```sh
bash apps/starry/net-bench/run.sh aarch64 slirp
bash apps/starry/net-bench/run.sh aarch64 slirp-smp4
bash apps/starry/net-bench/run.sh aarch64 tap
```

## 结果

| 场景 | TCP 1流 | TCP 4流 | UDP (target 1G) | 通过 |
|------|---------|---------|-----------------|------|
| slirp | | | | |
| slirp-smp4 | | | | |
| tap | | | | |

## 备注

- 是否出现 `NET_BENCH_PASSED`:
- 是否有 host 侧端口/监听地址问题:
- TAP host 配置:
~~~
