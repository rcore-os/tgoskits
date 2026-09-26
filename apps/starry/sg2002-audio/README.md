# SG2002 录音诊断

`sg2002-audio-check` 通过 Linux ALSA ioctl 检查板载单声道采集的参数、状态和资源释放。WAV 录制使用现成的 `arecord`。StarryOS 的 `sg2002-audio` feature 接入 ADC、I²S 和循环 DMA。

## 1. 构建

`audio-check.c` 使用目标 Linux 的 `sound/asound.h` 和 C 运行库；`CMakeLists.txt` 将程序安装到 `bin/sg2002-audio-check`。程序面向 64 位小端 Linux 用户态。

### 1.1 宿主检查

在仓库根目录构建并运行 `--abi-check`，对照 Linux C 头文件中的结构大小、字段偏移和 ioctl 编码。

```sh
cmake -S apps/starry/sg2002-audio -B /tmp/sg2002-audio-check -DCMAKE_BUILD_TYPE=Release
cmake --build /tmp/sg2002-audio-check
/tmp/sg2002-audio-check/sg2002-audio-check --abi-check
```

成功标记为 `ALSA_LP64_LAYOUT_OK`，与 `drivers/interface/alsa-pcm-uapi` 的 Rust 布局测试相互对照。

### 1.2 板端程序

板端需要 RISC-V64 Linux 用户态交叉编译器及匹配的 sysroot。板测目录 `test-suit/starryos/board-aka-00-sg2002/audio` 复用本目录的 CMake 目标，并通过会话文件部署程序。音频内核可用以下命令构建：

```sh
cargo xtask starry build -c os/StarryOS/configs/board/aka-00-sg2002.toml
```

`licheerv-nano-sg2002.toml` 也启用了同一音频 feature。

## 2. 板上检查

`open_capture()` 访问 `/dev/snd/pcmC0D0c`，`check_gain()` 读取 `/dev/snd/controlC0` 的单值 `ADC Capture Volume`。诊断程序应在 StarryOS 上独占声卡运行。

### 2.1 录音文件

使用 `arecord` 验证 alsa-lib 协商和实际 WAV 输出，并回放检查声音、削顶和采样速率。输出文件名应选择尚不存在的路径。

```sh
arecord -D hw:0,0 -c 1 -f S16_LE -r 16000 -d 5 -t wav mic-16k.wav
arecord -D hw:0,0 -c 1 -f S16_LE -r 48000 -d 5 -t wav mic-48k.wav
```

### 2.2 生命周期

`lifecycle()` 覆盖增益恢复、参数检查、独占打开、停止重启，以及 `dup/fork` 共享会话的最后引用释放。`check_blocking_and_overrun()` 检查非阻塞读取、信号打断、低于 `avail_min` 的读取进展、参数变更后的唤醒和溢出恢复。`check_drain()` 检查排空与停止后的读取状态；`check_geometry()` 验证最小可用 DMA 环的多次回绕。`capture_one_second()` 读取样本后查询 `SYNC_PTR/STATUS`。

```sh
timeout 30 ./sg2002-audio-check --lifecycle 16000
timeout 30 ./sg2002-audio-check --lifecycle 48000
```

全部步骤成功时输出 `SG2002_AUDIO_PASSED`；检查失败时输出 `SG2002_AUDIO_FAILED` 并返回非零。板端需要 `timeout` 命令，运行器按退出状态判定结果。录音文件和声音按第 2.1 节另行检查。

需要定向诊断时，可将 `--lifecycle` 替换为 `--blocking`、`--drain` 或 `--geometry`，分别只运行对应检查；采样率参数保持不变。

### 2.3 板卡运行器

`board-aka-00-sg2002/audio` 连续检查两档采样率，只有两次均通过才匹配总成功标记。下载诊断程序需要板端网络；`requirements.toml` 要求在本机环境设置 Wi-Fi 参数，勿将凭据写入仓库。

```sh
cargo xtask starry test board --list
cargo xtask starry test board -c board-aka-00-sg2002/audio --board aka-00-sg2002
```

运行器检查生命周期；使用 `arecord` 录制 WAV 后，还应试听并核对采样率。
