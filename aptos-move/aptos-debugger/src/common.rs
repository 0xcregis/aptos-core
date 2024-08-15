// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{execute_past_transactions, execute_pending_block};
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[clap(group(clap::ArgGroup::new("target")
        .required(true)
        .multiple(false)
        .args(&["rest_endpoint", "db_path"]),
))]
pub struct Target {
    /// Use full node's rest api as query endpoint.
    #[clap(long, group = "target")]
    pub(crate) rest_endpoint: Option<String>,

    /// Use a local db instance to serve as query endpoint.
    #[clap(long, group = "target")]
    pub(crate) db_path: Option<PathBuf>,
}

#[derive(Parser)]
pub struct Opts {
    #[clap(flatten)]
    pub(crate) target: Target,

    #[clap(long, num_args = 0..)]
    pub(crate) concurrency_level: Vec<usize>,

    #[clap(long, default_value_t = false)]
    pub(crate) enable_block_stm_profiling: bool,

    #[clap(long, default_value_t = true)]
    pub(crate) enable_committer_backup: bool,
}

#[derive(Parser)]
pub enum Command {
    ExecutePastTransactions(execute_past_transactions::Command),
    ExecutePendingBlock(execute_pending_block::Command),
}

impl Command {
    pub async fn run(self) -> Result<()> {
        match self {
            Command::ExecutePastTransactions(cmd) => cmd.run().await,
            Command::ExecutePendingBlock(cmd) => cmd.run().await,
        }
    }
}

#[test]
fn verify_tool() {
    use clap::CommandFactory;
    Command::command().debug_assert()
}
