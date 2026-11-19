# OrangePi捡球机器人场景

标准板卡测试保留在上一级；此目录只组织基于OrangePi的机器人场景。

| 场景 | 客户机 | 检查 |
| --- | --- | --- |
| `robot-virtual-linux` | Linux | 视觉/RKNN与FT232回环，28 FPS门槛，不驱动车轮或机械臂 |
| `robot-virtual-starry` | Starry | 同类视觉/RKNN与USB FTDI回环，28 FPS门槛 |
| `dual-linux-zephyr` | Linux＋Zephyr | 感知、IVC通信、Zephyr机器人控制流程 |
| `dual-starry-zephyr` | Starry＋Zephyr | Starry感知与Zephyr机器人控制流程 |

每个场景仍有自己的构建和板端配置、客户机配置及`tests/`检查。一次启动中的成员由`checks`按序引用；板端配置文件名保持，场景路径增加`robot/`，例如`board/orangepi-5-plus/robot/robot-virtual-linux`。原`--board orangepi-5-plus-robot-linux`选择名保持。

`common/guests/zephyr-dual/guest.toml`只由两个双客户机场景共享，不与标准板卡测试混放。目录移动不改变客户机镜像、CPU分配、设备所有权、业务门槛或CI登记。

两个双客户机流程涉及真实底盘和机械臂，执行前确认现场安全与板型/接线匹配；流程通过不等于实际夹球成功。构建、启动、资产和判定细节见各场景README及运行配置。

目录名中的`virtual`表示使用FT232回环模拟执行器，摄像头、NPU和客户机仍真实运行；不表示使用QEMU。双客户机真实执行器场景保持原名，独立的真实单客户机应用目录也保持原名。原`--board orangepi-5-plus-robot-linux`与`--board orangepi-5-plus-robot-starry`选择器保持，按场景完整路径选择时使用新的virtual目录名。
