# MADV_POPULATE 标准测试适配

## 1. 来源与范围

主程序复用 Linux **v6.12** 的
[`tools/testing/selftests/mm/madv_populate.c`](https://github.com/torvalds/linux/blob/v6.12/tools/testing/selftests/mm/madv_populate.c)，
使用同版本的 [`kselftest.h`](https://github.com/torvalds/linux/blob/v6.12/tools/testing/selftests/kselftest.h)。
后者保持原样；来源文件保留版权与 SPDX，许可证见 `upstream/COPYING`。
这些 GPL 用户态测试单独编译，不链接进内核。

已检查工作区当前采用的 LTP 20260529 `testcases/kernel/syscalls/madvise`
用例：没有直接覆盖 POPULATE_READ/WRITE 的用例，因此这里优先复用 kselftest，
不加入无关 LTP 用例，也不改动已合入的 WIPEONFORK 测试。

## 2. 适配差异

适配只改变缺失接口的观察方式和测试装配，不放宽保留下来的功能断言。

### 2.1 核心行为

`madv_populate.c` 的五组核心测试继续通过真实 madvise 调用观察结果，
完成计数由 kselftest 维护，Starry 运行器负责传播非零退出状态。

- 保留 `test_prot_read`、`test_prot_write`、`test_holes`、
  `test_populate_read`、`test_populate_write` 的原始操作和判定，共 **16 项**。
- 原始驻留观察依赖 `/proc/self/pagemap`，Starry 暂无该接口。
  这两个测试只使用新建的匿名私有映射，改用 `mincore` 逐页观察驻留状态；
  不把此替换推广到文件缓存、swap 或任意 PTE 状态，也不模拟 pagemap。
- 删除不再使用的 `vm_util.h` 依赖，调整本地头文件路径及 main 参数。
  完成时要求 16 项全部通过且没有跳过；功能探测退出 4 由系统运行器视为失败。

### 2.2 未覆盖接口

不移植依赖 pagemap/clear_refs 的 soft-dirty 子测试，因此**不是原版完整自测通过**。
此次不扩大内核 procfs 或 soft-dirty 支持范围；缺口也在程序输出中明确说明。
`range_has_residency` 只提供这里的匿名驻留观察，不提供 soft-dirty 或真实 PTE 查询。

## 3. 补充覆盖

`src/file-and-prefix.c` 只保留标准程序没有检查的已完成前缀、
零长度/对齐顺序、文件 EOF，以及私有/共享文件写入行为。
原自写匿名填充和 fork 测试移除；用户写入前解除 COW 的物理页/PTE 证明继续
由现有 `explicit_write_population_breaks_cow_before_userspace_writes` 内核测试承担。

## 4. 运行

共享 `system/prebuild.sh` 仅在选中本用例时安装 `linux-headers`，为上游源码提供 Linux UAPI 常量。沿用现有分组入口：

```sh
cargo xtask starry test qemu --arch x86_64 -c qemu/system/madvise-populate
```

同一目录安装两个程序，任何一个失败都会使分组失败。不能只凭补充程序的
成功标记判定整体通过；主程序必须完成 TAP 计划中的 16 项检查。
