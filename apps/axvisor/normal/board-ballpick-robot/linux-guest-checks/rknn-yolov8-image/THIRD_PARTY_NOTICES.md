# 第三方文件说明

本包内的RKNN调用和YOLO后处理代码来自Rockchip YOLOv8示例及其适配版本，源码文件头
保留原版权与Apache-2.0声明。`LICENSE`不改变第三方文件自身的授权条款。

- `3rdparty/rknpu2/`：Rockchip RKNN API头文件与AArch64运行库。API头文件带有厂商专有
  声明，运行库以二进制形式提供；本包不包含RKNN Runtime源码。分发或其他用途应遵守厂商授权。
- `3rdparty/librga/`：Rockchip RGA头文件与AArch64动态库，保留头文件自身的声明。
- `3rdparty/jpeg_turbo/`：TurboJPEG头文件、AArch64静态库和`LICENSE.md`。
  This software is based in part on the work of the Independent JPEG Group.
- `3rdparty/stb_image/`：STB图片处理头文件与`LICENSE.txt`。
- `model/`：现成的RK3588 RKNN模型与COCO标签，不包含模型训练代码或训练数据。

这些文件是本包内的独立副本，构建和运行不访问其他测试用例目录。

本包沿用仓库已有 `benchmarks/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/` 的第三方资源与公共工具代码，并按单张图片推理用途裁剪适配；可审计的仓库对照版本是本应用首次引入前的提交 `5207f372d1ed86a1966c97a46b0002aa630aca78`。上游来源是 Rockchip `rknn_model_zoo` 的 YOLOv8 示例及其适配版本；引入记录未保存精确上游提交号，不能据此宣称对应某个上游发行版。

两份目录共用来源，但分别维护：本包的 `src/` 负责图片目录推理入口，`utils/common.h`、`utils/file_utils.*`、`utils/image_utils.c` 和构建文件已有本用例适配，不能整目录覆盖。修改任一副本的第三方库、模型或公共工具代码时，提交者负责检查另一副本是否同样受影响：通用错误或安全修复应同步适用部分，用途差异应保留并在提交说明中写明不适用原因。更新运行库或模型须一起核对头文件、Runtime、驱动与模型兼容性，并分别验证 Linux 客户机和 Starry benchmark 的实际使用路径。这里没有自动同步或更新脚本。
