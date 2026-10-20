# Linux 客户机 NPU + YOLOv8 图片识别

## 用途

在RK3588 Linux客户机中，使用NPU执行YOLOv8图片推理，完整打印目录内每张图片的识别结果。
验证图片读取、RKNN模型执行和YOLO后处理，不比较预设答案，不验证识别准确率。
没有检测到目标时显示`Detections: 0`，仍可作为成功完成的推理。

## 在客户机中运行

本节只在已经启动的 Linux 客户机内执行，系统启动与资源部署见[上一级说明](../README.md)。

### 资源与权限

客户机中的 AxVisor 功能检查资源部署在`/guest/axvisor/`下，每个用例一个子目录。
将编译好的`install/rknn-yolov8-image/`整个目录放到`/guest/axvisor/npu/rknn-yolov8-image/`。
该目录在客户机内可直接访问，不需要sudo；
从板卡Linux侧写入时才需要，因为客户机与板卡Linux共用同一个根分区。
### 推理命令

把待识别的 JPG、JPEG、PNG 放进其中的 `images/`，然后执行：

```bash
cd /guest/axvisor/npu/rknn-yolov8-image
./yolov8_demo.sh
```

指定自己的图片目录（支持空格，不递归子目录）：

```bash
./yolov8_demo.sh /path/to/images
./yolov8_demo.sh "./my images"
```

默认使用配套YOLOv8n COCO 80类模型、NPU core 0、25%置信度阈值。调整阈值：

```bash
YOLO_MIN_CONFIDENCE=50 ./yolov8_demo.sh /path/to/images
```

Linux需要可工作的RKNPU驱动，且当前用户有访问该设备的权限。普通用户权限不足时，
可以执行`sudo ./yolov8_demo.sh`；本用例不自动修改设备权限或加载驱动。RKNN Runtime、模型、
驱动及用户态库必须相互兼容，不可随意替换为其他YOLO导出格式或不同芯片的模型。

## 客户机启动

本包只负责编译运行包和在客户机内运行推理，**不提供启动入口**：没有工作站启动器，也不做自动输入或自动判定。

AxVisor 与 Linux 客户机的启动命令、前置确认、以及各场景的完整检查清单见上一级目录的
[`../README.md`](../README.md)。在那里启动并进入客户机后，再回到本页「在客户机中运行」节执行推理命令。

包内的职责划分：

| 入口 | 执行位置 | 功能 |
| --- | --- | --- |
| `build.sh` | 工作站或板端编译环境 | 编译并整理运行包 |
| `yolov8_demo.sh` | Linux客户机 | 运行图片推理，不负责启动系统 |

## 结果判定

模型初始化一次，图片按文件名顺序处理。输出格式如下（数值为示例）：

```text
========== NPU YOLOv8 Image Inference ==========
Model: .../model/yolov8.rknn
Input directory: .../images
Images: 2
Confidence: 25.0%
NPU core: 0

[1/2] .../images/picture1.jpg
Image size: 640 x 640
Detections: 1
ID  Class ID  Class            Confidence  Box (left, top, right, bottom)
1   0         person            75.00%    (1, 2, 30, 40)
NPU run call: 10.000 ms
Preprocess + inference + postprocess: 15.000 ms
Status: PASS

... remaining images ...

========== Test Summary ==========
Processed: 2
Passed: 2
Failed: 0
YOLO_IMAGE_LIST_SUCCESS
success
```

每张图片打印全部达到阈值且经NMS保留的检测，包含类别ID、名称、置信度、检测框及耗时。
`NPU run call`是同步RKNN运行调用耗时，不是独立硬件性能指标。配套库可能额外输出模型
信息、图片解码信息或错误，不会将错误静默隐藏。

空目录、模型初始化失败、图片解码或推理失败均返回非零，不打印 `success`。单张图片失败
后继续处理其他图片，并计入最终失败数量。只有全部图片成功完成才打印 `success`。

## 编译运行包

本目录是独立完整的测试包，可以单独复制到其他位置或机器编译，不依赖TGOSKits其他目录。
工具链与输出目录均由包内脚本管理。

### 选择编译工具链

在工作站的本文档所在目录执行：

```bash
./build.sh
```

工作站交叉编译或AArch64 Linux原生编译都可以。默认在x86_64工作站优先选用安装好的
`aarch64-linux-gnu-gcc-11/g++-11`，否则使用`aarch64-linux-gnu-gcc/g++`；板端使用gcc/g++。
构建需要CMake及对应编译器。

使用自己的交叉编译器和sysroot：

```bash
CC=/path/to/aarch64-linux-gnu-gcc \
CXX=/path/to/aarch64-linux-gnu-g++ \
SYSROOT=/path/to/target-sysroot ./build.sh
```

编译环境应与客户机用户态兼容；例如较新系统编译的程序可能依赖旧客户机没有的GLIBC
版本。出现`GLIBC_x.y not found`时，用匹配客户机的sysroot重新构建，或在同版本板端Linux
原生编译，不应替换客户机系统libc。更换工具链时为CMake选择新的构建目录或清理旧构建缓存。

### 部署完整运行包

`CMakeLists.txt` 将程序、运行库、模型和许可文件安装到同一目录，部署时保持该相对布局：

```text
install/rknn-yolov8-image/
├── yolov8_demo.sh
├── rknn_yolov8_images
├── lib/
├── model/
├── images/
├── licenses/
└── README.md
```

完整复制这个目录即可部署；客户机不需要源码、CMake或编译器。模型和图片是真正的运行
输入，并非每次演示产生的输出。替换图片不需要重编译，修改程序后需要重新编译并部署。

## 源码与依赖

本包以仓库 `benchmarks/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/` 中的第三方资源和公共工具代码为基础，按 Linux 客户机图片推理用途裁剪适配，保留独立的构建与部署目录。两份代码已有用途差异；更新通用错误修复、第三方库或模型时，提交者需检查并同步另一副本的适用部分，不能直接覆盖整个目录。来源对照提交、上游版本记录边界与同步责任见源码目录的 `THIRD_PARTY_NOTICES.md`；安装后的文件位于运行包 `licenses/THIRD_PARTY_NOTICES.md`。

| 内容 | 作用 |
| --- | --- |
| `src/main.cc` | 图片遍历、模型生命周期、逐图调用、英文结果和汇总 |
| `src/rknpu2/yolov8.cc`、`src/postprocess.*` | RKNN推理调用与YOLO后处理 |
| `utils/` | 图片读取、解码、缩放与补边等处理 |
| `3rdparty/` | 配套RKNN/RGA头文件和AArch64库、JPEG及STB依赖 |
| `model/` | 配套RK3588 YOLOv8模型及80类标签 |
| `CMakeLists.txt`、`build.sh` | 编译、依赖链接及整理运行包 |
| `yolov8_demo.sh` | 客户机入口，配置运行库路径并启动图片识别 |
| `images/` | 可替换的演示图片 |

上述源码、头文件、库、模型、图片和许可文件均随包提供，构建只使用包内路径。
没有指向其他用例的软链接，也不需要旧example目录。包不包含摄像头、视频流或预设答案
比较程序。独立构建仍需要系统安装编译器、CMake等开发工具；不将编译器和Linux内核打入测试包。
