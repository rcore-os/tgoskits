// Copyright 2025 The tgoskits Team
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

//! ArceOS build commands for xtask

use anyhow::Result;
use clap::Subcommand;

pub mod build;
pub mod run;

pub use build::{BuildArgs, run_build};
pub use run::{RunArgs, run_with_arg, test_with_arg_in_scope};

/// ArceOS subcommands
#[derive(Subcommand, Debug)]
pub enum ArceosCommand {
    /// Build ArceOS application
    Build {
        #[command(flatten)]
        args: BuildArgs,
    },
    /// Build and run ArceOS application
    Run {
        #[command(flatten)]
        args: RunArgs,
    },
}

impl ArceosCommand {
    pub async fn run(self) -> Result<()> {
        match self {
            ArceosCommand::Build { args } => run_build(args).await,
            ArceosCommand::Run { args } => run_with_arg(args).await,
        }
    }
}
