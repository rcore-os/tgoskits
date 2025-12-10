# RVlwext4

一个用 Rust 重写的简易 ext4 实现，旨在提升性能与安全性。

## 功能特性

- **mkfs**: 创建 ext4 文件系统
- **mount**: 挂载文件系统
- **umount**: 卸载文件系统
- **readat**: 从指定位置读取数据
- **writeat**: 向指定位置写入数据
- **Journal**: 实现了mode为的ordered的Journal日志功能,在mount时会自动进行replay。
## 测试方法

要测试项目，请执行以下步骤：

```bash
cd example/qemu_virt
make run
