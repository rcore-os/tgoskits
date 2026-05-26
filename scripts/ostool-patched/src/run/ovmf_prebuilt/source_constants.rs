use super::Source;

#[allow(missing_docs)]
impl Source {
    pub const EDK2_STABLE202408_R1: Self = Self {
        tag: "edk2-stable202408-r1",
        sha256: "63a9217ddd51fa45d0a89fd83c483cc971765de6bb08e83cf70836b0baff0d48",
    };

    pub const EDK2_STABLE202408_01_R1: Self = Self {
        tag: "edk2-stable202408.01-r1",
        sha256: "1b4c7d7603517482a3c4461ba43044c4c7e0a7930274d77eb19600b7dcd9b838",
    };

    pub const EDK2_STABLE202411_R1: Self = Self {
        tag: "edk2-stable202411-r1",
        sha256: "963fc6cef6a0560cec97381ed22a7d5c76f440c8212529a034cb465466cd57cc",
    };

    pub const EDK2_STABLE202502_R1: Self = Self {
        tag: "edk2-stable202502-r1",
        sha256: "6d6122e88cdc09e1ffafb6a39fbdbfba668a6ded3f2a032b2cd6c0b7ff6d69df",
    };

    pub const EDK2_STABLE202502_R2: Self = Self {
        tag: "edk2-stable202502-r2",
        sha256: "dd59d3d52f0a643f07a488f80ab40f89c30f360999d98cdffb30e1eba5476641",
    };

    pub const EDK2_STABLE202505_R2: Self = Self {
        tag: "edk2-stable202505-r2",
        sha256: "ed4e5502dc6b4e1d524929b27b274f9d6e2581a4179612b5628de9660008c8e0",
    };

    pub const EDK2_STABLE202508_R1: Self = Self {
        tag: "edk2-stable202508-r1",
        sha256: "e461e2f0a43092ef9d29996ba0f8c05de097223791c33f5ff61a7e1ea12e6dac",
    };

    /// Latest release tag.
    ///
    /// Note that this is not necessarily the latest prebuilt available
    /// from the git repo.
    pub const LATEST: Self = Self::EDK2_STABLE202508_R1;
}
