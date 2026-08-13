# AIC8800 firmware

These are AICSemi vendor firmware blobs for the AIC8800 series Wi-Fi chip
(MAC firmware, patches, and patch tables). They are **not** committed to this
repository.

## Provisioning

The crate build script resolves the blobs on demand and verifies them
byte-for-byte against pinned SHA-256 digests before writing them to Cargo's
`OUT_DIR`. This directory is an optional local cache for offline builds; put
verified blobs here, or set `AIC8800_FIRMWARE_DIR` to an external cache. See
[`../build.rs`](../build.rs) for the file manifest, digests, and source pin.

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
| `fmacfw_patch_8800dc_h_u02.bin` | `aic8800DC/fmacfw_patch_8800dc_h_u02.bin` |
| `fmacfw_patch_tbl_8800dc_h_u02.bin` | `aic8800DC/fmacfw_patch_tbl_8800dc_h_u02.bin` |
| `fmacfw_calib_8800dc_h_u02.bin` | `aic8800DC/fmacfw_calib_8800dc_h_u02.bin` |
| `fmacfw_8800d80_u02.bin` | `aic8800_and_aic8800D80/fmacfw_8800d80_u02.bin` |
| `fw_patch_8800d80_u02.bin` | `aic8800_and_aic8800D80/fw_patch_8800d80_u02.bin` |
| `fw_patch_table_8800d80_u02.bin` | `aic8800_and_aic8800D80/fw_patch_table_8800d80_u02.bin` |

## AIC8800DC RF config tables

The AIC8800DC LDPC / AGC / TX-gain tables (`FW_DC_LDPC_CFG`, `FW_DC_AGC_CFG`,
`FW_DC_TXGAIN_MAP`, `FW_DC_TXGAIN_MAP_H`) are **not** firmware images and have no
upstream firmware mirror — they are little-endian `u32` arrays from the vendor
BSP source `aic8800dc_compat.c`, inlined as Rust byte arrays in
`src/fw/firmware/dc_rf_cfg.rs`, so no `.bin` blob is kept here for them.
