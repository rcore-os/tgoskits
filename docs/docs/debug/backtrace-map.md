---
sidebar_position: 6
sidebar_label: "Backtrace map 故障排查"
---

# Backtrace map 故障排查

运行时不依赖宿主机 ELF、DWARF 或 `addr2line`。构建阶段生成的 `AXBT`
文件必须和最终内核 ELF 使用相同的架构、text 地址和 build-id，并随
initramfs 放在 `/symbols/kernel.axbt`。StarryOS 同时使用同一构建生成的
`/symbols/kernel.axks`，该 sidecar 保存 `/proc/kallsyms` 所需的 T/t、D/d、B/b、R/r
类型和地址；缺少 AXKS 时只保留 AXBT 文本符号回退。

如果输出只有地址，按以下顺序检查：

1. initramfs 是否包含 `/symbols/kernel.axbt`，以及文件是否在归档回收前
   被复制成功。
2. StarryOS 的 `/proc/kallsyms` 是否同时包含 `/symbols/kernel.axks`；缺少该
   sidecar 时 kmod/kprobe 只能解析 AXBT 文本符号，数据、BSS 和只读数据符号
   不会出现在 procfs 中。
3. map 的架构和链接地址是否对应当前 QEMU/板卡目标。
4. `BACKTRACE=y` 是否保留帧指针；没有可用帧链时只能得到首帧或
   `BT_ERROR`。
5. map 的记录是否覆盖报错地址。text 区间之外的地址会安全降级为原始
   `ip/fp`。

极早期启动阶段尚未解包 initramfs，地址级输出是预期行为。文件系统就绪
后，新的 panic/trap 会使用已安装 map 输出函数与 `file:line`。StarryOS
的 `/proc/kallsyms`、kprobe 和 kmod 通过 target-owned AXKS/AXBT provider
解析内核符号；名字查询在启动时建立索引，避免加载模块时扫描整个地址表；用户态
回溯使用自己的地址空间 map。
