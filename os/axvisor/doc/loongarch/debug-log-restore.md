# LoongArch 调试日志恢复补丁

`restore-debug-logs.patch` 保存了一组 LoongArch LVZ 启动/VM-exit 定位用日志。

正常开发时不应用该补丁，避免启动日志变慢、变多。需要重新定位启动卡顿、中断、GSPR/IOCSR、timer、guest PC 推进等问题时，在仓库根目录执行：

```bash
git apply os/axvisor/doc/loongarch/restore-debug-logs.patch
```

定位完成后移除这些调试日志：

```bash
git apply -R os/axvisor/doc/loongarch/restore-debug-logs.patch
```

补丁已用 `git apply --check` 验证可应用到当前代码。
