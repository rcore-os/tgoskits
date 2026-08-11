# AxVisor ivshmem IVC SDK

这个目录提供 AxVisor ivshmem/vPCI 设备之上的客户机消息通信 SDK。AxVisor 负责向客户机暴露标准 ivshmem 设备、BAR0 doorbell 和 BAR2 共享内存；消息格式、双向 ring、请求/回复匹配、图像数据描述和控制命令都由客户机侧 SDK 完成。

SDK 对外提供 C ABI，C 和 C++ 用户程序都可以直接 include `ivc_sdk.h` 使用。Linux 和 Zephyr 的示例程序都是 C++ 入口，底层 SDK 和 platform backend 仍用 C 编译并链接。

## 目录结构

```text
common/
  include/        SDK 公共头文件，业务程序主要 include ivc_sdk.h
  src/            消息协议、ring、client、图像/控制命令封装
linux/
  init.cpp        Linux C++ 示例程序
  src/            Linux platform backend，负责发现 PCI 设备并 mmap BAR0/BAR2
zephyr/
  CMakeLists.txt  Zephyr 示例构建文件
  prj.conf        Zephyr 示例配置，启用 C++
  src/            Zephyr C++ 示例程序和 platform backend
tests/
  *.c             SDK 在 host 上运行的协议/队列/请求回复单元测试
```

`common` 是可复用 SDK 核心；`linux/src/ivc_platform.c` 和 `zephyr/src/ivc_platform.c` 是当前示例使用的默认平台适配层。业务代码不需要关心 PCI vendor/device、BAR 地址、doorbell 寄存器或共享内存布局。

## 基本使用方式

C 或 C++ 业务程序只需要包含：

```c
#include "ivc_sdk.h"
```

打开默认连接：

```c
struct ivc_sdk sdk = {0};

if (ivc_sdk_open_default(&sdk, IVC_PEER_LINUX) != IVC_OK) {
    /* 处理错误 */
}
```

Zephyr 侧使用：

```c
if (ivc_sdk_open_default(&sdk, IVC_PEER_ZEPHYR) != IVC_OK) {
    /* 处理错误 */
}
```

使用结束后调用：

```c
ivc_sdk_close(&sdk);
```

## 发送图像

Zephyr 采集到图像后，可以把图像数据写入共享内存的数据区，并通过消息 ring 发送图像描述符：

```c
struct ivc_sdk_image image = {
    .image_id = 42,
    .width = 1024,
    .height = 256,
    .pixel_format = IVC_PIXEL_FORMAT_GRAY8,
    .data = image_buf,
    .data_len = image_len,
};
uint64_t seq = 0;

ivc_sdk_send_image(&sdk, &image, now_ms, 1000, &seq);
```

`seq` 是这条请求消息的序号，后续对端回复时会通过 `reply_to` 指向它。

## 接收图像并释放

Linux 收到图像消息后，先解析描述符，再按需把图像数据拷贝到自己的缓冲区。处理完成后应释放共享内存中的图像数据块，避免长时间通信时耗尽数据区。

```c
struct ivc_message msg = {
    .payload = payload,
    .payload_capacity = sizeof(payload),
};
struct ivc_sdk_received_image image;

ivc_sdk_recv(&sdk, &msg, IVC_RECV_NO_WAIT);
ivc_sdk_recv_image(&msg, &image);
ivc_sdk_read_image(&sdk, &image, local_buf, sizeof(local_buf));
ivc_sdk_release_image(&sdk, &image);
```

## 控制命令和请求对应

Linux 处理图像后，可以向 Zephyr 发送控制命令，并把 `reply_to` 设置为收到的图像消息 `seq`：

```c
struct ivc_sdk_control control = {
    .command = IVC_CMD_SET_EXPOSURE,
    .target_id = image.image_id,
    .args = "apply",
    .arg_len = 6,
};
uint64_t control_seq = 0;

ivc_sdk_send_control(&sdk, &control, msg.header.seq, now_ms, 1000,
                     &control_seq);
```

SDK 会把待回复请求记录到 pending table 中。收到结果消息后，调用 `ivc_sdk_complete_reply()` 可以确认这条回复对应哪条请求：

```c
struct ivc_pending_entry completed;
struct ivc_sdk_control_result_view result;

ivc_sdk_complete_reply(&sdk, &reply_msg, &completed);
ivc_sdk_recv_control_result(&reply_msg, &result);
```

这样 Zephyr 连续发送多帧图像、Linux 连续返回多条命令或结果时，双方仍然可以通过 `seq` / `reply_to` 和 `user_data` 判断消息对应关系。

## Zephyr 执行策略

当前示例采用简单策略：Zephyr 收到 Linux 的控制命令后立即执行，然后返回 `CONTROL_DONE` 或 `CONTROL_FAILED`。如果业务需要排队、取消、超时或优先级调度，可以在 SDK 之上再封装业务状态机。

## 构建集成

集成时通常需要编译并链接这些文件：

```text
common/src/ivc_ring.c
common/src/ivc_client.c
common/src/ivc_demo.c
common/src/ivc_sdk.c
<platform>/src/ivc_platform.c
<your_app>.c 或 <your_app>.cpp
```

C++ 业务程序建议仍然让 `common/src/*.c` 和 `ivc_platform.c` 使用 C 编译器编译，最后用 C++ 编译器链接。`ivc_sdk.h` 已经提供 `extern "C"`，可以直接被 C++ 包含。
