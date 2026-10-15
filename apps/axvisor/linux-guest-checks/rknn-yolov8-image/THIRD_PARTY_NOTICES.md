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
