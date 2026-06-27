# Starry Network Test

`network` 是一个 StarryOS 应用级网络能力测试。它验证默认 virtio-net 网卡 `eth0` 的地址展示、`ifconfig` ioctl 配置路径、`ip` netlink 配置路径，以及到 QEMU user-net 主机网关的连通性。

## 覆盖能力

- `ifconfig` 查看全局和 `eth0` 状态。
- `ip addr show` / `ip link show eth0` 查看 netlink 视角。
- `ifconfig eth0 10.0.2.15 netmask 255.255.255.0 up`、`down`、`up`、`0.0.0.0` 覆盖 ioctl 配置路径。
- `ip addr add/del 10.0.2.15/24 dev eth0` 和 `ip link set eth0 down/up` 覆盖 netlink 配置路径。
- 交叉验证连通性随网卡状态变化：有地址且 up 时能 ping 网关，down 或删除地址后不能 ping，恢复后再次能 ping。
- `ping -c 3 10.0.2.2` 验证 guest 到 QEMU host gateway 网络连通。
- 在地址删改测试前执行 `ping -c 3 www.baidu.com`，验证初始网络的 DNS 解析和外网 ICMP 连通。

## 运行方式

```bash
cargo xtask starry app qemu -t network --arch x86_64
```

QEMU 配置使用 user networking：

```text
guest eth0: 10.0.2.15/24
gateway:    10.0.2.2
external:   www.baidu.com
```

成功输出包含：

```text
NETWORK_STAGE_IOCTL_DONE
NETWORK_STAGE_NETLINK_DONE
NETWORK_STAGE_EXTERNAL_PING_DONE
NETWORK_STAGE_PING_DONE
NETWORK_TEST_PASSED
```
