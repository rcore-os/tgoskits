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

use ostool::{Tool, ToolConfig};

use super::BuildArgs;

pub struct Context {
    pub tool: Tool,
    repo_root: PathBuf,
    pub build_config_path: Option<std::path::PathBuf>,
    pub vmconfigs: Vec<String>,
}

impl Context {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        let repo_root = repo_root.as_ref().to_path_buf();
        let tool = init_tool(&repo_root, None, None);

        Context {
            tool,
            repo_root,
            build_config_path: None,
            vmconfigs: vec![],
        }
    }

    pub fn apply_build_args(&mut self, args: &BuildArgs) {
        self.tool = init_tool(&self.repo_root, args.build_dir.clone(), args.bin_dir.clone());
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}

fn init_tool(repo_root: &Path, build_dir: Option<PathBuf>, bin_dir: Option<PathBuf>) -> Tool {
    Tool::new(ToolConfig {
        manifest: Some(repo_root.to_path_buf()),
        build_dir: build_dir.clone(),
        bin_dir: bin_dir.clone(),
        ..Default::default()
    })
    .or_else(|_| {
        Tool::new(ToolConfig {
            build_dir,
            bin_dir,
            ..Default::default()
        })
    })
    .expect("failed to initialize ostool Tool")
}
