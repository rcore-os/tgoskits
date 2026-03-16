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

//! ArceOS build library
//!
//! This library provides the core functionality for building ArceOS applications.
//! It supports multiple architectures and platforms, and can be used both
//! as a library and as a command-line tool.

pub mod arceos;

pub use arceos::{
    build::{BuildOutput, Builder, PreparedArtifacts, prepare_artifacts},
    config::{
        AVAILABLE_BOARDS, AXCONFIG_FILE_NAME, ArceosConfig, ArceosConfigOverride, Arch, BuildMode,
        CONFIG_FILE_NAME, LogLevel, NetDev, OSTOOL_EXTRA_CONFIG_FILE_NAME, QEMU_CONFIG_FILE_NAME,
        QemuOptions, apply_defconfig, axconfig_path, config_path, load_board_config, load_config,
        ostool_extra_config_path, parse_qemu_options, qemu_config_path, resolve_package_app_dir,
        save_config,
    },
    features::FeatureResolver,
    platform::{PlatformInfo, PlatformResolver},
    qemu::QemuRunner,
};
