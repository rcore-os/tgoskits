---
sidebar_position: 11
sidebar_label: "Backtrace 与行号 map"
---

# Backtrace 与行号 map

TGOSKits 的 backtrace 在 target 内完成符号化。构建机从最终 ELF/DWARF
生成 `AXBT` map，并把它放入 initramfs 的 `/symbols/kernel.axbt`。内核
解包 initramfs 后复制并校验 map，再回收归档页；panic、trap、alloc 和
普通回溯因此都可以直接输出函数名与 `file:line`。map 尚未就绪的极早期
故障仍输出地址级帧。

## 运行时格式

`components/axbacktrace` 提供无分配解析器和全局 map provider。AXBT v1
包含 magic、版本、架构、build-id、text 地址范围、排序的函数区间以及
NUL 结尾的函数名和文件名字符串。解析器拒绝截断头、错误架构、越界区间、
无效 UTF-8 和未排序记录；地址查询使用有界二分查找。

输出继续使用稳定的协议：

```text
BACKTRACE_BEGIN kind=panic arch=x86_64 alloc=false map=true
BT 0 ip=0xffff800000123456 fp=0xffff800001001000 symbol=panic_handler at kernel/src/panic.rs:42
BACKTRACE_END
```

没有 map 时 `BT` 行只包含 `ip` 和 `fp`，不会阻塞错误处理。`BACKTRACE=y`
仍负责保留帧指针；`DWARF=y` 只影响构建期 map 生成，不要求 target 在线
解析 DWARF。

## 构建和打包

ArceOS、StarryOS 和 Axvisor 的最终 ELF 都经过同一套 map 生成接口。map
和不可变启动资源通过 initramfs/bundle 一起打包。Axvisor 的 guest 资源
使用 `/guest/builtin/configs`、`images` 和 `symbols`；可写客户机磁盘、
模型、标定及用户数据必须由 manifest 显式声明，默认留在外部路径。

## 测试

QEMU 测试直接检查 target 串口中的函数名和 `file:line`。不再需要
宿主 ELF、`addr2line`、QEMU 后处理日志或 host symbolizer。调试时保存完整串口输出即可复现 target 自己看到的错误。
