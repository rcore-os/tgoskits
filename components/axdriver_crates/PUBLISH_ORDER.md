# 發布至 crates.io 順序

依賴關係決定發布順序，請按下列順序執行 `cargo publish`：

1. **axdriver_base**（無內部依賴）
2. **axdriver_pci**（無內部依賴）
3. **axdriver_block**、**axdriver_display**、**axdriver_net**（僅依賴 base，可任意順序）
4. **axdriver_virtio**（依賴 base、block、display、net，最後發布）

`cargo publish --dry-run` 僅在依賴已存在於 crates.io 時會通過。因此：

- **axdriver_base**、**axdriver_pci**：現在即可 `cargo publish --dry-run` 通過。
- **axdriver_block**、**axdriver_display**、**axdriver_net**：需先發布 **axdriver_base** 後，再執行 dry-run 才會通過。
- **axdriver_virtio**：需先發布 base、block、display、net 後，再執行 dry-run 才會通過。

本地開發時，根目錄的 `[patch.crates-io]` 會將上述 crate 指到本地路徑，`cargo build --workspace` 可正常編譯。
