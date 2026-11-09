# LoongArch测试兼容入口

unaligned-fixup的唯一实现已移入`../cpu/src/unaligned_fixup.rs`，默认由CPU的loong-standard批次执行，不再单独启动。

```bash
cargo xtask arceos test qemu --arch loongarch64 --test-group cpu --test-case unaligned-fixup
```

旧`--test-group loongarch`命令仍转发到同一成员，供手动兼容使用；CI不同时调用新旧两条入口。添加或修改测试遵循[CPU维护规范](../cpu/README.md)。
