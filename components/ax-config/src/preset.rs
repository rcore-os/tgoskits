use crate::{CommonConfig, LogLevel, RuntimeOptions};

/// Cross-OS board identity, without OS-specific driver or resource policy.
#[derive(Debug)]
pub struct Board {
    pub id: &'static str,
    pub target: &'static str,
}

/// Defaults selected by an OS's static recipe.
#[derive(Debug, Clone, Copy)]
pub struct CommonDefaults {
    pub log: LogLevel,
    pub max_cpu_num: Option<usize>,
    pub runtime: RuntimeOptions,
}

impl CommonDefaults {
    /// Common unspecified inputs, without resolving runtime fallback values.
    pub const DEFAULT: Self = Self {
        log: LogLevel::Warn,
        max_cpu_num: None,
        runtime: RuntimeOptions {
            task_stack_size: None,
            scheduler_tick_ms: None,
        },
    };
}

/// Borrowed compile-time recipe, expanded once per request.
#[derive(Debug, Clone, Copy)]
pub struct CommonPreset {
    pub board: &'static Board,
    pub defaults: CommonDefaults,
    pub feature_sets: &'static [&'static [&'static str]],
    pub rustflags: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
}

impl CommonPreset {
    /// Start a recipe with no features, flags or environment entries.
    pub const fn new(board: &'static Board, defaults: CommonDefaults) -> Self {
        Self {
            board,
            defaults,
            feature_sets: &[],
            rustflags: &[],
            env: &[],
        }
    }

    /// Select borrowed feature fragments in expansion order.
    pub const fn with_feature_sets(mut self, sets: &'static [&'static [&'static str]]) -> Self {
        self.feature_sets = sets;
        self
    }

    /// Select environment entries; later duplicate keys win during expansion.
    pub const fn with_env(mut self, env: &'static [(&'static str, &'static str)]) -> Self {
        self.env = env;
        self
    }

    /// Select complete rustc arguments, retaining order and duplicates.
    pub const fn with_rustflags(mut self, flags: &'static [&'static str]) -> Self {
        self.rustflags = flags;
        self
    }

    /// Expand features in first-seen order, retain all flags, and apply env last-wins.
    pub fn materialize(&self) -> CommonConfig {
        let mut features: Vec<String> = Vec::new();
        for set in self.feature_sets {
            for feature in *set {
                if !features.iter().any(|item| item.as_str() == *feature) {
                    features.push((*feature).to_owned());
                }
            }
        }
        CommonConfig {
            target: Some(self.board.target.to_owned()),
            features,
            rustflags: self
                .rustflags
                .iter()
                .map(|flag| (*flag).to_owned())
                .collect(),
            env: self
                .env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            log: self.defaults.log,
            max_cpu_num: self.defaults.max_cpu_num,
            runtime: self.defaults.runtime,
        }
    }
}
