use super::{AXSTD_STD_CLIPPY_FEATURES, AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ClippyCheckKind {
    Base,
    Feature(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ClippyDepsMode {
    NoDeps,
    WithDeps,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ClippyCheck {
    pub(super) package: String,
    pub(super) kind: ClippyCheckKind,
    pub(super) deps_mode: ClippyDepsMode,
    pub(super) target: Option<String>,
    pub(super) env: Vec<(String, String)>,
}

impl ClippyCheck {
    pub(super) fn cargo_args(&self) -> Vec<String> {
        let mut args = match &self.kind {
            ClippyCheckKind::Base => vec!["clippy".into(), "-p".into(), self.package.clone()],
            ClippyCheckKind::Feature(feature) => vec![
                "clippy".into(),
                "-p".into(),
                self.package.clone(),
                "--no-default-features".into(),
                "--features".into(),
                feature.clone(),
            ],
        };
        if self.package == AXSTD_STD_PACKAGE
            && matches!(&self.kind, ClippyCheckKind::Feature(feature) if feature == AXSTD_STD_DEFAULT_FEATURE)
        {
            args = vec![
                "clippy".into(),
                "-p".into(),
                self.package.clone(),
                "--no-default-features".into(),
                "--features".into(),
                AXSTD_STD_CLIPPY_FEATURES.into(),
            ];
        }
        if matches!(self.deps_mode, ClippyDepsMode::NoDeps) {
            args.insert(1, "--no-deps".into());
        }
        if let Some(target) = &self.target {
            args.extend(["--target".into(), target.clone()]);
        }
        args.extend(["--".into(), "-D".into(), "warnings".into()]);
        args
    }

    pub(super) fn label(&self) -> String {
        let base = match &self.kind {
            ClippyCheckKind::Base => format!("{} (base", self.package),
            ClippyCheckKind::Feature(feature) => {
                format!("{} (feature: {}", self.package, feature)
            }
        };

        match &self.target {
            Some(target) => format!("{base}, target: {target})"),
            None => format!("{base})"),
        }
    }

    pub(super) fn env_prefix(&self) -> String {
        self.env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
