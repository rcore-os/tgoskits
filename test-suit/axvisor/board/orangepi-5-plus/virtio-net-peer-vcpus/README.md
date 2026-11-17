# 24客户机虚拟网络测试

一次OrangePi 5 Plus宿主启动，运行24个ArceOS客户机，每个客户机一个vCPU、128 MiB内存。8个物理核各固定承载3个客户机，总客户机内存3 GiB；不允许vCPU跨物理核迁移。

客户机使用不同的IP和MAC，形成VM1→VM2→…→VM24通信链。VM1检查发送心跳并收到VM2应答，VM2至VM24检查接收心跳。48条串口步骤依次切换和检查客户机，全部完成才通过。这是功能与调度压力测试，没有吞吐或延迟达标阈值；当前未登记普通CI、夜间或benchmark。

## 文件职责

| 文件 | 作用 |
| --- | --- |
| `build-aarch64-unknown-none-softfloat.toml` | 宿主构建、8个宿主CPU及24份生成的客户机配置 |
| `board-orangepi-5-plus-virtio-net-peer-vcpus.toml` | 板卡启动、3600秒预算、失败条件和检查引用 |
| `tests/network-vcpus.toml` | 原48条串口检查，执行顺序不变 |
| `guest-builds.toml` | 声明标准运行入口需要先准备客户机资源 |
| `orangepi-arceos-virtio-net-test.sh` | 生成描述、逐个构建带独立身份的客户机、提供手动入口 |

真正的客户机程序仍在 `apps/arceos/virtio-net-peer`；24份配置由脚本生成到被忽略的 `target/axvisor-vcpus-gen`，不手写24份重复配置。客户机镜像通过运行时host initramfs提供，不嵌入 `axvisor.bin`。

## 使用

标准入口会自动生成配置、构建24份客户机、构建并打包宿主，然后申请板卡执行：

```sh
cargo xtask axvisor test board \
  --board orangepi-5-plus-virtio-net-peer-vcpus \
  --server <HOST> --port <PORT>
```

只查看登记，不准备资源或申请板卡：

```sh
cargo xtask axvisor test board \
  --board orangepi-5-plus-virtio-net-peer-vcpus --list
```

脚本保留以下诊断入口：

```sh
# 仅生成配置并核对与构建/检查清单的一致性
bash test-suit/axvisor/board/orangepi-5-plus/virtio-net-peer-vcpus/orangepi-arceos-virtio-net-test.sh gen

# 仅准备24份客户机，供guest-builds.toml调用
bash test-suit/axvisor/board/orangepi-5-plus/virtio-net-peer-vcpus/orangepi-arceos-virtio-net-test.sh guests

# 准备客户机及宿主构建产物
bash test-suit/axvisor/board/orangepi-5-plus/virtio-net-peer-vcpus/orangepi-arceos-virtio-net-test.sh build

# 标准板卡服务入口，前置构建只执行一次
bash test-suit/axvisor/board/orangepi-5-plus/virtio-net-peer-vcpus/orangepi-arceos-virtio-net-test.sh run --server <HOST> --port <PORT>
```

`uboot`模式保留原直接U-Boot启动方式，可传递该入口支持的`--uboot-config`等参数。默认`run`使用板卡服务，不修改本机或CI的默认服务地址。构建输出遵循前置构建提供的`AXBUILD_TARGET_DIR`，`LLVM_OBJCOPY`可显式指定工具，否则从当前Rust宿主工具链定位。

## 增改约束

- 改客户机数量时同步脚本、构建中的配置清单及串口检查；准备阶段先核对配置数和步骤数，不一致即失败。
- 每份客户机的身份编入二进制，必须分别构建并及时保存，不能把最后一次构建复制成24份。
- 同一运行环境新增兼容检查应复用当前启动，保持原有网络链路、CPU分布与内存压力。
- 不通过减少客户机、CPU、断言或改变绑定来制造通过。跨核迁移不在本场景已证明的范围内。
- 运行记录应包含板卡/会话身份、版本、镜像哈希及24个客户机的实际检查结果；只列出或只构建不算通过。
