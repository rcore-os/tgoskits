use anyhow::Context as _;

use crate::ctx::Context;

impl Context {
    pub async fn run_vmconfig(&mut self) -> anyhow::Result<()> {
        let json = schemars::schema_for!(axvmconfig::AxVMCrateConfig);
        std::fs::write(
            ".vmconfig-schema.json",
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .with_context(|| "Failed to write schema file .vmconfig-schema.json")?;
        Ok(())
    }
}
