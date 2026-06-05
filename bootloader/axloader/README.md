# axloader

`axloader` provides the reusable AxVisor HTTP Boot loader library and the UEFI
loader binary.

Build a loader by selecting exactly one `board-*` feature and the matching UEFI
target:

```bash
cargo build -p axloader \
  --target x86_64-unknown-uefi \
  --features board-asus-nuc15crh \
  --bin axloader \
  --release
```

The output path follows Cargo's target directory layout, for example:

```text
target/x86_64-unknown-uefi/release/axloader.efi
```

Board features select both the board profile and its architecture feature:

```text
board-asus-nuc15crh -> arch-x86_64
```

To add a new board, add its `board-*` feature, board module under
`src/boards/`, `BootloaderTarget` entry in `src/target.rs`, and the matching
entry in `build.rs`. A LoongArch64 board should use a LoongArch64 UEFI target
and enable `arch-loongarch64`.
