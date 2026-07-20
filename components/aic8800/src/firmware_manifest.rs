//! Pinned AIC8800 firmware provisioning manifest shared by the crate build
//! script and the workspace build tool.

pub const REPOSITORY: &str = "lxowalle/aic8800-sdio-firmware";
pub const COMMIT: &str = "c56f910044cc854d6c553bcb9a644f3bca5a4c38";

pub struct FirmwareFile {
    pub name: &'static str,
    pub remote_path: &'static str,
    pub sha256: &'static str,
}

pub const FILES: &[FirmwareFile] = &[
    FirmwareFile {
        name: "fmacfw.bin",
        remote_path: "aic8800_and_aic8800D80/fmacfw.bin",
        sha256: "2c6e70726df10ef74d9b1a657c74fdcfaeb88855b96b2c9bc8e0e603ac7c4cc3",
    },
    FirmwareFile {
        name: "fmacfw_patch.bin",
        remote_path: "aic8800_and_aic8800D80/fmacfw_patch.bin",
        sha256: "6c8126ad655e9971f05ca03dc60fa82cb6d48c3b02cf3ba960137566ce2e28d5",
    },
    FirmwareFile {
        name: "fmacfw_patch_8800dc_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_8800dc_u02.bin",
        sha256: "69d3ac2038da3b8e652ed1ec5079598ceb6df51db7b87b1d33f6d3c820c86a6f",
    },
    FirmwareFile {
        name: "fw_patch_8800dc_u02.bin",
        remote_path: "aic8800DC/fw_patch_8800dc_u02.bin",
        sha256: "c4087b95e788785df0fc55aa92152d214323ee028c70ba0ebb23944d4070340b",
    },
    FirmwareFile {
        name: "fw_patch_table_8800dc_u02.bin",
        remote_path: "aic8800DC/fw_patch_table_8800dc_u02.bin",
        sha256: "e7eea12cc85fca5d8667182b4520b6a0929044c70c6d9e9a3d7ece8b16169688",
    },
    FirmwareFile {
        name: "fmacfw_patch_tbl_8800dc_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_tbl_8800dc_u02.bin",
        sha256: "62d53a223eda1ea064ba82a6fe67829d0720e9f4e87d26763fd13316ccd2a90b",
    },
    FirmwareFile {
        name: "fmacfw_patch_8800dc_h_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_8800dc_h_u02.bin",
        sha256: "f388dcb419a0f677c777a1eaad798156eabdfbb72c512a4d993df0dbc4f351d1",
    },
    FirmwareFile {
        name: "fmacfw_patch_tbl_8800dc_h_u02.bin",
        remote_path: "aic8800DC/fmacfw_patch_tbl_8800dc_h_u02.bin",
        sha256: "0469686691b72fa8296ff7abd1669ba978bdc0f115137fd392aa00a2717ff887",
    },
    FirmwareFile {
        name: "fmacfw_calib_8800dc_h_u02.bin",
        remote_path: "aic8800DC/fmacfw_calib_8800dc_h_u02.bin",
        sha256: "12bdcdd48e41b33bfd74834bffa326b4469bea82e7134de079392fbc2508acc7",
    },
    FirmwareFile {
        name: "fmacfw_8800d80_u02.bin",
        remote_path: "aic8800_and_aic8800D80/fmacfw_8800d80_u02.bin",
        sha256: "ffb49ede6004e58453f01489edf28b888b509529c3173554c98aa94fbb33507d",
    },
    FirmwareFile {
        name: "fw_patch_8800d80_u02.bin",
        remote_path: "aic8800_and_aic8800D80/fw_patch_8800d80_u02.bin",
        sha256: "f0e2f5bbc17bc327ca7f1574ff55370dfd863d931514347bb4abc18a74f6218f",
    },
    FirmwareFile {
        name: "fw_patch_table_8800d80_u02.bin",
        remote_path: "aic8800_and_aic8800D80/fw_patch_table_8800d80_u02.bin",
        sha256: "9decb77435b7e9713e33e32da483d683b7329ed93b672b2d1b134031d7da5f67",
    },
];
