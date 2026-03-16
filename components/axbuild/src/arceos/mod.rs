// Copyright 2025 The tgoskits Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod build;
pub mod config;
pub mod features;
pub mod ostool;
pub mod platform;
pub mod qemu;

pub use build::{BuildOutput, Builder, PreparedArtifacts, prepare_artifacts};
pub use config::{
    AVAILABLE_BOARDS, AXCONFIG_FILE_NAME, ArceosConfig, ArceosConfigOverride, Arch, BuildMode,
    CONFIG_FILE_NAME, LogLevel, NetDev, OSTOOL_EXTRA_CONFIG_FILE_NAME, QEMU_CONFIG_FILE_NAME,
    QemuOptions, apply_defconfig, axconfig_path, axconfig_path_for_config, config_path,
    load_board_config, load_config, ostool_extra_config_path, parse_qemu_options, qemu_config_path,
    qemu_config_path_for_config, resolve_package_app_dir, save_config,
};
pub use features::FeatureResolver;
pub use platform::{CpuInfo, PlatformInfo, PlatformResolver};
pub use qemu::QemuRunner;
