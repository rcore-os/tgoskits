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

use std::collections::HashSet;

use crate::arceos::config::{ArceosConfig, LogLevel};

/// Feature resolver
pub struct FeatureResolver;

impl FeatureResolver {
    /// Default lib features for axlibc (C applications)
    pub const DEFAULT_LIBC_FEATURES: &[&str] = &[
        "fp-simd",
        "smp",
        "irq",
        "alloc",
        "multitask",
        "fs",
        "net",
        "fd",
        "pipe",
        "select",
        "epoll",
    ];

    /// All available features
    pub const ALL_FEATURES: &[&str] = &[
        // Platform
        "defplat",
        "myplat",
        "plat-dyn",
        // Library
        "fp-simd",
        "irq",
        "alloc",
        "multitask",
        "fs",
        "net",
        "fd",
        "pipe",
        "select",
        "epoll",
        // Logging
        "log-level-off",
        "log-level-error",
        "log-level-warn",
        "log-level-info",
        "log-level-debug",
        "log-level-trace",
        // Other
        "sched-affinity",
        "sched-fifo",
        "deadline",
    ];

    /// Check if a feature is a lib feature (for axlibc/axstd)
    pub fn is_lib_feature(feat: &str) -> bool {
        matches!(
            feat,
            "fp-simd"
                | "smp"
                | "irq"
                | "alloc"
                | "multitask"
                | "fs"
                | "net"
                | "fd"
                | "pipe"
                | "select"
                | "epoll"
                | "sched-affinity"
                | "sched-fifo"
                | "deadline"
        )
    }

    /// Resolve ax_features (ArceOS module features)
    pub fn resolve_ax_features(config: &ArceosConfig, plat_dyn: bool) -> Vec<String> {
        let mut features = Vec::new();

        // Platform-related features
        if plat_dyn {
            features.push("plat-dyn".to_string());
        } else if config.platform.starts_with("myplat") {
            features.push("myplat".to_string());
        } else {
            features.push("defplat".to_string());
        }

        // User-specified features (non-lib features)
        for feat in &config.features {
            if !Self::is_lib_feature(feat) {
                features.push(feat.clone());
            }
        }

        features.sort();
        features.dedup();
        features
    }

    /// Resolve lib features for a specific library
    pub fn resolve_lib_features(config: &ArceosConfig, lib_name: &str) -> Vec<String> {
        let mut features = Vec::new();

        if config.smp.unwrap_or(1) > 1 {
            features.push("smp".to_string());
        }

        // C application (axlibc) includes default lib features
        if lib_name == "axlibc" {
            for feat in Self::DEFAULT_LIBC_FEATURES {
                if config.features.contains(&feat.to_string()) {
                    features.push(feat.to_string());
                }
            }
        }

        // User-specified lib features
        for feat in &config.features {
            if Self::is_lib_feature(feat) {
                features.push(feat.clone());
            }
        }

        features.sort();
        features.dedup();
        features
    }

    /// Resolve log level feature
    pub fn resolve_log_feature(log_level: LogLevel) -> String {
        match log_level {
            LogLevel::Off => "log-level-off".to_string(),
            LogLevel::Error => "log-level-error".to_string(),
            LogLevel::Warn => "log-level-warn".to_string(),
            LogLevel::Info => "log-level-info".to_string(),
            LogLevel::Debug => "log-level-debug".to_string(),
            LogLevel::Trace => "log-level-trace".to_string(),
        }
    }

    /// Check if a feature is valid
    pub fn is_valid_feature(feat: &str) -> bool {
        Self::ALL_FEATURES.contains(&feat) || feat.starts_with("custom-")
    }

    /// Validate all features in config
    pub fn validate_features(features: &[String]) -> Vec<String> {
        let mut invalid = Vec::new();

        for feat in features {
            if !Self::is_valid_feature(feat) {
                invalid.push(feat.clone());
            }
        }

        invalid
    }

    /// Parse features from comma-separated string
    pub fn parse_features(features_str: &str) -> Vec<String> {
        features_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Merge feature sets, removing duplicates
    pub fn merge_features(features: &[Vec<String>]) -> Vec<String> {
        let mut set = HashSet::new();
        for list in features {
            for feat in list {
                set.insert(feat.clone());
            }
        }
        let mut result: Vec<_> = set.into_iter().collect();
        result.sort();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arceos::config::{ArceosConfig, Arch, BuildMode, LogLevel, QemuOptions};

    #[test]
    fn test_is_lib_feature() {
        assert!(FeatureResolver::is_lib_feature("fp-simd"));
        assert!(FeatureResolver::is_lib_feature("smp"));
        assert!(FeatureResolver::is_lib_feature("net"));
        assert!(!FeatureResolver::is_lib_feature("defplat"));
        assert!(!FeatureResolver::is_lib_feature("myplat"));
    }

    #[test]
    fn test_resolve_log_feature() {
        assert_eq!(
            FeatureResolver::resolve_log_feature(LogLevel::Info),
            "log-level-info"
        );
        assert_eq!(
            FeatureResolver::resolve_log_feature(LogLevel::Debug),
            "log-level-debug"
        );
    }

    #[test]
    fn test_resolve_ax_features() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            platform: "aarch64-qemu-virt".to_string(),
            mode: BuildMode::Debug,
            log: LogLevel::Info,
            smp: None,
            mem: None,
            features: vec!["fs".to_string(), "net".to_string()],
            app_features: vec![],
            qemu: QemuOptions::default(),
        };

        // Non-dynamic platform, so "defplat" is used
        let ax_features = FeatureResolver::resolve_ax_features(&config, false);
        assert!(ax_features.contains(&"defplat".to_string()));
        assert!(!ax_features.contains(&"fs".to_string())); // lib features are not included
        assert!(!ax_features.contains(&"net".to_string()));
    }

    #[test]
    fn test_resolve_lib_features() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            platform: "aarch64-qemu-virt".to_string(),
            mode: BuildMode::Debug,
            log: LogLevel::Info,
            smp: None,
            mem: None,
            features: vec!["fs".to_string(), "net".to_string(), "fp-simd".to_string()],
            app_features: vec![],
            qemu: QemuOptions::default(),
        };

        let lib_features = FeatureResolver::resolve_lib_features(&config, "axlibc");
        assert!(lib_features.contains(&"fs".to_string()));
        assert!(lib_features.contains(&"net".to_string()));
        assert!(lib_features.contains(&"fp-simd".to_string()));
    }

    #[test]
    fn test_resolve_lib_features_enables_smp_when_cpu_count_gt_one() {
        let mut config = ArceosConfig {
            arch: Arch::AArch64,
            platform: "aarch64-qemu-virt".to_string(),
            mode: BuildMode::Debug,
            log: LogLevel::Info,
            smp: Some(4),
            mem: None,
            features: vec![],
            app_features: vec![],
            qemu: QemuOptions::default(),
        };

        let lib_features = FeatureResolver::resolve_lib_features(&config, "axstd");
        assert!(lib_features.contains(&"smp".to_string()));

        config.smp = Some(1);
        let lib_features = FeatureResolver::resolve_lib_features(&config, "axstd");
        assert!(!lib_features.contains(&"smp".to_string()));
    }

    #[test]
    fn test_resolve_ax_features_with_dynamic_platform() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            platform: "aarch64-qemu-virt".to_string(),
            mode: BuildMode::Debug,
            log: LogLevel::Info,
            smp: None,
            mem: None,
            features: vec![],
            app_features: vec![],
            qemu: QemuOptions::default(),
        };

        let ax_features = FeatureResolver::resolve_ax_features(&config, true);
        assert!(ax_features.contains(&"plat-dyn".to_string()));
        assert!(!ax_features.contains(&"defplat".to_string()));
    }

    #[test]
    fn test_resolve_ax_features_with_myplat() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            platform: "myplat-custom".to_string(),
            mode: BuildMode::Debug,
            log: LogLevel::Info,
            smp: None,
            mem: None,
            features: vec![],
            app_features: vec![],
            qemu: QemuOptions::default(),
        };

        let ax_features = FeatureResolver::resolve_ax_features(&config, false);
        assert!(ax_features.contains(&"myplat".to_string()));
        assert!(!ax_features.contains(&"defplat".to_string()));
    }

    #[test]
    fn test_parse_features() {
        let features = FeatureResolver::parse_features("fs,net,fp-simd");
        assert_eq!(features, vec!["fs", "net", "fp-simd"]);
    }

    #[test]
    fn test_merge_features() {
        let result = FeatureResolver::merge_features(&[
            vec!["fs".to_string(), "net".to_string()],
            vec!["net".to_string(), "fp-simd".to_string()],
        ]);
        assert_eq!(result, vec!["fp-simd", "fs", "net"]);
    }
}
