use crate::ctx::Context;
use crate::tbuld::Config;
use jkconfig::{ElemHock, ui::components::editors::show_feature_select};
use std::sync::Arc; // HashMap is unused

impl Context {
    /// Main menuconfig runner function
    pub async fn run_menuconfig(&mut self) -> anyhow::Result<()> {
        println!("Configure runtime parameters");
        let config_path = self.ctx.workspace_folder.join(".build.toml");
        if config_path.exists() {
            println!("\nCurrent .build.toml configuration file: {}", config_path.display());
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
        let cargo_toml = self.ctx.workspace_folder.join("Cargo.toml");
        ElemHock {
            path: path.to_string(),
            callback: Arc::new(move |siv: &mut jkconfig::cursive::Cursive, _path: &str| {
                show_feature_select(siv, &package_name, &cargo_toml, None);
            }),
        }
    }
}
