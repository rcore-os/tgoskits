use super::{
    AXSTD_STD_CLIPPY_FEATURES, AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE, HOST_TEST_FEATURE,
};

pub(super) struct ClippyCargoInvocation {
    pub(super) args: Vec<String>,
    pub(super) env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ClippyCheckKind {
    Base,
    Feature(String),
    Configuration {
        name: String,
        features: Vec<String>,
        rustflags: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ClippyCheck {
    pub(super) package: String,
    pub(super) kind: ClippyCheckKind,
    pub(super) target: Option<String>,
    pub(super) env: Vec<(String, String)>,
}

impl ClippyCheck {
    pub(super) fn cargo_args(&self) -> Vec<String> {
        self.cargo_args_for_target(self.target.as_deref())
    }

    fn cargo_args_for_target(&self, target: Option<&str>) -> Vec<String> {
        let mut args = match &self.kind {
            ClippyCheckKind::Base => vec![
                "clippy".into(),
                "--no-deps".into(),
                "-p".into(),
                self.package.clone(),
            ],
            ClippyCheckKind::Feature(feature) => {
                let mut args = vec![
                    "clippy".into(),
                    "--no-deps".into(),
                    "-p".into(),
                    self.package.clone(),
                ];
                if feature == HOST_TEST_FEATURE {
                    args.push("--tests".into());
                }
                args.extend([
                    "--no-default-features".into(),
                    "--features".into(),
                    feature.clone(),
                ]);
                args
            }
            ClippyCheckKind::Configuration { features, .. } => {
                let mut args = vec![
                    "clippy".into(),
                    "--no-deps".into(),
                    "-p".into(),
                    self.package.clone(),
                ];
                if !features.is_empty() {
                    args.extend(["--features".into(), features.join(",")]);
                }
                args
            }
        };
        if self.package == AXSTD_STD_PACKAGE
            && matches!(&self.kind, ClippyCheckKind::Feature(feature) if feature == AXSTD_STD_DEFAULT_FEATURE)
        {
            args = vec![
                "clippy".into(),
                "--no-deps".into(),
                "-p".into(),
                self.package.clone(),
                "--no-default-features".into(),
                "--features".into(),
                AXSTD_STD_CLIPPY_FEATURES.into(),
            ];
        }
        if let Some(target) = target {
            args.extend(["--target".into(), target.to_string()]);
        }
        args.push("--".into());
        if let ClippyCheckKind::Configuration { rustflags, .. } = &self.kind {
            args.extend(rustflags.clone());
        }
        args.extend(["-D".into(), "warnings".into()]);
        args
    }

    pub(super) fn cargo_invocation(&self) -> ClippyCargoInvocation {
        let Some(target) = self.target.as_deref() else {
            return ClippyCargoInvocation {
                args: self.cargo_args(),
                env: self.env.clone(),
            };
        };
        let Some(target) = crate::build::bare_build_target_for(target) else {
            return ClippyCargoInvocation {
                args: self.cargo_args(),
                env: self.env.clone(),
            };
        };
        let mut args = self.cargo_args_for_target(Some(&target.target));
        let rustc_args_index = args
            .iter()
            .position(|arg| arg == "--")
            .expect("clippy arguments must delimit rustc flags");
        args.splice(rustc_args_index..rustc_args_index, target.cargo_args);

        let mut env = self.env.clone();
        for (key, value) in target.env {
            if let Some((_, existing)) = env.iter_mut().find(|(existing, _)| existing == &key) {
                *existing = value;
            } else {
                env.push((key, value));
            }
        }
        env.sort();

        ClippyCargoInvocation { args, env }
    }

    pub(super) fn label(&self) -> String {
        let base = match &self.kind {
            ClippyCheckKind::Base => format!("{} (base", self.package),
            ClippyCheckKind::Feature(feature) => {
                format!("{} (feature: {}", self.package, feature)
            }
            ClippyCheckKind::Configuration { name, features, .. } => format!(
                "{} (configuration: {}, features: {}",
                self.package,
                name,
                features.join(",")
            ),
        };

        match &self.target {
            Some(target) => format!("{base}, target: {target})"),
            None => format!("{base})"),
        }
    }

    pub(super) fn env_prefix(&self) -> String {
        self.cargo_invocation()
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
