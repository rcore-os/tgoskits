// Copyright 2025 The Axvisor Team
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

use std::path::{Path, PathBuf};

use ostool::ctx::{AppContext, PathConfig};

use super::BuildArgs;

pub struct Context {
    pub ctx: AppContext,
    repo_root: PathBuf,
    pub build_config_path: Option<std::path::PathBuf>,
    pub vmconfigs: Vec<String>,
}

impl Context {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        let repo_root = repo_root.as_ref().to_path_buf();
        let ctx = AppContext {
            paths: PathConfig {
                workspace: repo_root.clone(),
                manifest: repo_root.clone(),
                ..Default::default()
            },
            ..Default::default()
        };

        Context {
            ctx,
            repo_root,
            build_config_path: None,
            vmconfigs: vec![],
        }
    }

    pub fn apply_build_args(&mut self, args: &BuildArgs) {
        self.ctx.paths.config.build_dir = args.build_dir.clone();
        self.ctx.paths.config.bin_dir = args.bin_dir.clone();
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}
