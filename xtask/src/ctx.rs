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

use ostool::ctx::{AppContext, PathConfig};

use crate::BuildArgs;

pub struct Context {
    pub ctx: AppContext,
    pub build_config_path: Option<std::path::PathBuf>,
    pub vmconfigs: Vec<String>,
}

impl Context {
    pub fn new() -> Self {
        let workdir = std::env::current_dir().expect("Failed to get current working directory");

        let ctx = AppContext {
            paths: PathConfig {
                workspace: workdir.clone(),
                manifest: workdir,
                ..Default::default()
            },
            ..Default::default()
        };

        Context {
            ctx,
            build_config_path: None,
            vmconfigs: vec![],
        }
    }

    pub fn apply_build_args(&mut self, args: &BuildArgs) {
        self.ctx.paths.config.build_dir = args.build_dir.clone();
        self.ctx.paths.config.bin_dir = args.bin_dir.clone();
    }
}
