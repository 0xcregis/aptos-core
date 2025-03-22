// Copyright (c) The Aptos Labs
// Copyright (c) The Move Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Args;

use std::path::Path;

/// Holds the commands that we support while querying the bytecode.
#[derive(Copy, Clone, Debug, Args)]
#[group(id = "cmd", required = false, multiple = false)]
pub struct QuerierOptions {
    #[arg(long, group = "cmd")]
    dump_call_graph: bool,

    #[arg(long, group = "cmd")]
    dump_dep_graph: bool,
}

/// Represents an instance of a querier. The querier can ...
pub struct Querier {
    options: QuerierOptions,
}


impl Querier {
    /// Creates a new querier.
    pub fn new(options: QuerierOptions) -> Self {
        Self {
            options,
        }
    }

    ///Main interface to run the query actions
    pub fn query(&self, bytecode_path: &Path) -> Result<String> {
        Ok("cfg".to_string())
    }

}
