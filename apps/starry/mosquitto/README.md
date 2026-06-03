# Starry Mosquitto App

This case runs Mosquitto MQTT broker inside StarryOS through the app runner.

## Mosquitto功能介绍

Mosquitto是一个轻量级的MQTT消息代理（broker），实现了MQTT协议3.1/3.1.1/5.0版本。

### 核心功能：
1. **消息代理** - 作为MQTT broker，负责接收、路由和分发消息
2. **发布/订阅模式** - 支持发布者（publisher）和订阅者（subscriber）模式
3. **主题路由** - 支持多级主题（如 `sensor/room1/temperature`）和通配符订阅（`+` 单级、`#` 多级）
4. **QoS支持** - 支持三种QoS级别：
   - QoS 0: 最多一次（fire and forget）
   - QoS 1: 至少一次（acknowledged delivery）
   - QoS 2: 恰好一次（assured delivery）
5. **保留消息** - 支持retained messages，新订阅者立即收到最后一条消息
6. **持久化会话** - 支持持久化客户端会话，断线重连后可恢复
7. **认证授权** - 支持用户名/密码认证和ACL访问控制
8. **WebSocket支持** - 可通过WebSocket协议提供MQTT服务
9. **集群支持** - 支持多broker桥接和集群

### 典型应用场景：
- 物联网（IoT）设备通信
- 传感器数据收集
- 智能家居系统
- 实时消息推送
- 移动应用后端消息服务

## 运行命令

默认运行全部测试（smoke + normal + stress）：

```bash
cargo xtask starry app run -t mosquitto --arch x86_64
```

也可以单独运行某个级别：

```bash
# 仅 Smoke 测试
cargo xtask starry app run -t mosquitto --arch x86_64 --qemu-config qemu-x86_64-smoke.toml

# 仅普通测试
cargo xtask starry app run -t mosquitto --arch x86_64 --qemu-config qemu-x86_64-normal.toml

# 仅压力测试
cargo xtask starry app run -t mosquitto --arch x86_64 --qemu-config qemu-x86_64-stress.toml
```

## 测试内容

### Smoke测试
- 基本发布/订阅
- 多主题测试
- 通配符订阅
- QoS级别测试

### 普通测试
- 基本发布/订阅
- 多消息测试
- 通配符主题
- QoS 0/1/2级别
- 保留消息
- 持久化会话
- 大消息负载
- 多客户端并发
- 主题特殊字符

### 压力测试
- 高频消息发布/订阅
- 多客户端并发压力
- 大消息压力测试
- 通配符压力测试
- 混合QoS压力测试
- 持久化重启测试

## 文件结构

```
apps/starry/mosquitto/
├── prebuild.sh                    # 构建脚本，安装mosquitto到rootfs
├── test_mosquitto.sh              # 统一入口，按序运行全部测试
├── mosquitto-smoke-tests.sh       # Smoke测试脚本
├── mosquitto-tests.sh             # 普通测试脚本
├── mosquitto-stress-tests.sh      # 压力测试脚本
├── build-*.toml                   # 构建配置
├── qemu-x86_64.toml               # 默认配置（运行全部测试）
├── qemu-x86_64-smoke.toml         # 仅Smoke测试
├── qemu-x86_64-normal.toml        # 仅普通测试
├── qemu-x86_64-stress.toml        # 仅压力测试
└── README.md                      # 本文件
```

## 依赖

- mosquitto MQTT broker
- mosquitto-clients (mosquitto_pub, mosquitto_sub)
