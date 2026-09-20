# AIC8800 firmware

These are AICSemi vendor firmware blobs for the AIC8800 series Wi-Fi chip
(MAC firmware, patches, and patch tables). They are **not** committed to this
repository.

## Provisioning

The crate build script resolves the blobs on demand and verifies them
byte-for-byte against pinned SHA-256 digests before writing them to Cargo's
`OUT_DIR`. A valid blob already present in `OUT_DIR` is reused. For offline
builds, put verified blobs in this directory or set `AIC8800_FIRMWARE_DIR` to
an external cache. See [`../build.rs`](../build.rs) for the file manifest,
digests, and source pin.

## Source

Fetched from the upstream firmware package referenced by the LicheeRV Nano
buildroot recipe `aic8800-sdio-firmware`:

- Repository: <https://github.com/lxowalle/aic8800-sdio-firmware>
- Pinned commit: `c56f910044cc854d6c553bcb9a644f3bca5a4c38`

The `aic8800` crate embeds the following blobs via `include_bytes!`
(`src/firmware/mod.rs`):

| File | Upstream path |
|------|---------------|
| `fmacfw_patch_8800dc_u02.bin` | `aic8800DC/fmacfw_patch_8800dc_u02.bin` |
| `fmacfw_patch_tbl_8800dc_u02.bin` | `aic8800DC/fmacfw_patch_tbl_8800dc_u02.bin` |
| `fmacfw_patch_8800dc_h_u02.bin` | `aic8800DC/fmacfw_patch_8800dc_h_u02.bin` |
| `fmacfw_patch_tbl_8800dc_h_u02.bin` | `aic8800DC/fmacfw_patch_tbl_8800dc_h_u02.bin` |
| `fmacfw_calib_8800dc_u02.bin` | `aic8800DC/fmacfw_calib_8800dc_u02.bin` |
| `fmacfw_calib_8800dc_h_u02.bin` | `aic8800DC/fmacfw_calib_8800dc_h_u02.bin` |
| `fmacfw_patch_8800dc_hbt_u02.bin` | `aic8800DC/fmacfw_patch_8800dc_hbt_u02.bin` |
| `fmacfw_patch_tbl_8800dc_hbt_u02.bin` | `aic8800DC/fmacfw_patch_tbl_8800dc_hbt_u02.bin` |
| `fmacfw_calib_8800dc_hbt_u02.bin` | `aic8800DC/fmacfw_calib_8800dc_hbt_u02.bin` |
| `fmacfw_8800d80_u02.bin` | `aic8800_and_aic8800D80/fmacfw_8800d80_u02.bin` |
