# AIC8800 firmware

These are AICSemi vendor firmware blobs for the AIC8800 series Wi-Fi chip
(MAC firmware, patches, and patch tables). They are **not** committed to this
repository.

## Provisioning

The blobs are fetched on demand by the build tooling and verified byte-for-byte
against pinned SHA-256 digests before use. Any `cargo xtask starry ...` or
`cargo xtask clippy ...` invocation that compiles the `aic8800` crate downloads
them into this directory automatically. See
[`scripts/axbuild/src/firmware.rs`](../../../scripts/axbuild/src/firmware.rs)
for the file manifest, digests, and source pin.

## Source

Fetched from the upstream firmware package referenced by the LicheeRV Nano
buildroot recipe `aic8800-sdio-firmware`:

- Repository: <https://github.com/lxowalle/aic8800-sdio-firmware>
- Pinned commit: `c56f910044cc854d6c553bcb9a644f3bca5a4c38`

The `aic8800` crate embeds the following blobs via `include_bytes!`
(`src/fw/firmware/data.rs`):

| File | Upstream path |
|------|---------------|
| `fmacfw.bin` | `aic8800_and_aic8800D80/fmacfw.bin` |
| `fmacfw_patch.bin` | `aic8800_and_aic8800D80/fmacfw_patch.bin` |
| `fmacfw_patch_8800dc_u02.bin` | `aic8800DC/fmacfw_patch_8800dc_u02.bin` |
| `fw_patch_8800dc_u02.bin` | `aic8800DC/fw_patch_8800dc_u02.bin` |
| `fw_patch_table_8800dc_u02.bin` | `aic8800DC/fw_patch_table_8800dc_u02.bin` |
| `fmacfw_8800d80_u02.bin` | `aic8800_and_aic8800D80/fmacfw_8800d80_u02.bin` |
| `fw_patch_8800d80_u02.bin` | `aic8800_and_aic8800D80/fw_patch_8800d80_u02.bin` |
| `fw_patch_table_8800d80_u02.bin` | `aic8800_and_aic8800D80/fw_patch_table_8800d80_u02.bin` |
