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
