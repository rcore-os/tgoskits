# 演示图片

`picture1.jpg`沿用原`starry-contest/demo/yolov8/examples/yolov8/picture/picture1.jpg`。
可以将自己的 JPG、JPEG、PNG 放入本目录；运行只读取当前目录，不递归扫描子目录。新增图片后重新部署 `images/` 即可，不需要重新编译；也可用 `yolov8_demo.sh /path/to/images` 直接指定客户机中的图片目录。
图片不包含预设识别结果，程序也不进行准确率比较。

构建、部署和输出判定见[推理包说明](../README.md)。空目录会返回失败，不能用没有图片的运行作为通过证据。
