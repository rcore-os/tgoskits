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

use crate::ctx::Context;
use crate::tbuld::Config;
use jkconfig::{ElemHock, ui::components::editors::show_feature_select};
use std::sync::Arc; // HashMap is unused

impl Context {
    /// Main menuconfig runner function
    pub async fn run_menuconfig(&mut self) -> anyhow::Result<()> {
        println!("Configure runtime parameters");

        let config_path = self.ctx.paths.workspace.join(".build.toml");
        if config_path.exists() {
            println!(
                "\nCurrent .build.toml configuration file: {}",
                config_path.display()
            );
        } else {
            println!("\nNo .build.toml configuration file found, will use default configuration");
        }

        let Some(_c): Option<Config> =
            jkconfig::run(config_path, true, &[self.default_package_feature_select()]).await?
        else {
            return Err(anyhow::anyhow!("Menuconfig was cancelled"));
        };
        Ok(())
    }

    pub fn default_package_feature_select(&self) -> ElemHock {
        let path = "features";
        let package_name = "axvisor".to_string();

        let cargo_toml = self.ctx.paths.workspace.join("Cargo.toml");
        ElemHock {
            path: path.to_string(),
            callback: Arc::new(move |siv: &mut jkconfig::cursive::Cursive, _path: &str| {
                show_feature_select(siv, &package_name, &cargo_toml, None);
            }),
        }
    }
}
