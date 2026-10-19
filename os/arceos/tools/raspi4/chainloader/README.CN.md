# 教程06 - UART链加载器

## tl;dr

- 从SD卡上运行是一次不错的体验，但是每次都为每个新的二进制文件这样做将非常繁琐。
  因此，让我们编写一个[chainloader]。
- 这将是您需要放在SD卡上的最后一个二进制文件。
  每个后续的教程都将在`Makefile`中提供一个`chainboot`，让您方便地通过`UART`加载内核。

[chainloader]: https://en.wikipedia.org/wiki/Chain_loading


## 注意

请注意，这个教程中有一些内容仅通过查看源代码很难理解。

大致的意思是，在`boot.s`中，我们编写了一段[position independent code]代码，
它会自动确定固件加载二进制文件的位置（`0x8_0000`），以及链接到的位置（`0x200_0000`，参见 `kernel.ld`）。
然后，二进制文件将自身从加载地址复制到链接地址（也就是"重定位"自身），然后跳转到`_start_rust()`的重定位版本。

由于链加载程序现在已经"脱离了路径"，它现在可以从`UART`接收另一个内核二进制文件，并将其复制到RPi固件的标准加载地址`0x8_0000`。
最后，它跳转到`0x8_0000`，新加载的二进制文件会透明地执行，就好像它一直从SD卡加载一样。

在我有时间详细写下这些内容之前，请耐心等待。目前，请将这个教程视为一种便利功能的启用程序，它允许快速启动以下教程。
_对于那些渴望深入了解的人，可以直接跳到上游 [`rust-raspberrypi-OS-tutorials`](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials) 的第 15 章，阅读 README 的前半部分，
其中讨论了`Load Address != Link Address`的问题_。

[position independent code]: https://en.wikipedia.org/wiki/Position-independent_code

## 安装并测试它

我们的链加载程序称为`MiniLoad`，受到了[raspbootin]的启发。

您可以按照以下教程尝试它：
1. 根据您的目标硬件运行命令：`make`或`BSP=rpi4 make`。
1. 将`kernel8.img`复制到SD卡中，并将SD卡重新插入您的RPi。
1. 运行命令`make chainboot`或`BSP=rpi4 make chainboot`。
1. 将USB串口连接到您的主机PC上。
    - 请参考 [ArceOS Raspberry Pi 4 指南](../../../doc/platform_raspi4.md) 了解板卡启动背景。
    - 确保您**没有**连接USB串口的电源引脚，只连接RX/TX和GND。
1. 将RPi连接到（USB）电源线。
1. 观察加载程序通过`UART`获取内核：

> ❗ **注意**: `make chainboot`假设默认的串行设备名称为`/dev/ttyUSB0`。根据您的主机操作系统，设备名称可能会有所不同。
> 例如，在`macOS`上，它可能是类似于`/dev/tty.usbserial-0001`的名称。
> 在这种情况下，请明确给出设备名称：


```console
$ DEV_SERIAL=/dev/tty.usbserial-0001 make chainboot
```

[raspbootin]: https://github.com/mrvn/raspbootin

```console
$ make chainboot
[...]
Minipush 1.0

[MP] ⏳ Waiting for /dev/ttyUSB0
[MP] ✅ Serial connected
[MP] 🔌 Please power the target now

 __  __ _      _ _                 _
|  \/  (_)_ _ (_) |   ___  __ _ __| |
| |\/| | | ' \| | |__/ _ \/ _` / _` |
|_|  |_|_|_||_|_|____\___/\__,_\__,_|

           Raspberry Pi 3

[ML] Requesting binary
[MP] ⏩ Pushing 7 KiB ==========================================🦀 100% 0 KiB/s Time: 00:00:00
[ML] Loaded! Executing the payload now

[0] mingo version 0.5.0
[1] Booting on: Raspberry Pi 3
[2] Drivers loaded:
      1. BCM PL011 UART
      2. BCM GPIO
[3] Chars written: 117
[4] Echoing input now
```

在这个教程中，为了演示目的，加载了上一个教程中的内核版本。在后续的教程中，将使用工作目录的内核。

## 测试它

这个教程中的`Makefile`有一个额外的目标`qemuasm`，它可以让你很好地观察到内核在重新定位后如何从加载地址区域（`0x80_XXX`）
跳转到重新定位的代码（`0x0200_0XXX`）：

```console
$ make qemuasm
[...]
N:
0x00080030:  58000140  ldr      x0, #0x80058
0x00080034:  9100001f  mov      sp, x0
0x00080038:  58000141  ldr      x1, #0x80060
0x0008003c:  d61f0020  br       x1

----------------
IN:
0x02000070:  9400044c  bl       #0x20011a0

----------------
IN:
0x020011a0:  90000008  adrp     x8, #0x2001000
0x020011a4:  90000009  adrp     x9, #0x2001000
0x020011a8:  f9446508  ldr      x8, [x8, #0x8c8]
0x020011ac:  f9446929  ldr      x9, [x9, #0x8d0]
0x020011b0:  eb08013f  cmp      x9, x8
0x020011b4:  54000109  b.ls     #0x20011d4
[...]
```

## 相比之前的变化（diff）
请检查[英文版本](README.md#diff-to-previous)，这是最新的。
