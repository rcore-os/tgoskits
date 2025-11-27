use ostool::ctx::AppContext;

pub struct Context {
    pub ctx: AppContext,
    pub build_config_path: Option<std::path::PathBuf>,
    pub vmconfigs: Vec<String>,
}

impl Context {
    pub fn new() -> Self {
        let workdir = std::env::current_dir().expect("Failed to get current working directory");

        let ctx = AppContext {
            manifest_dir: workdir.clone(),
            workspace_folder: workdir,
            ..Default::default()
        };
        Context {
            ctx,
            build_config_path: None,
            vmconfigs: vec![],
        }
    }
}
