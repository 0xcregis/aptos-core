// Copyright (c) The Aptos Labs
// Copyright (c) The Move Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Parser;

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    str,
};

/// Holds the actions that we support while querying the bytecode.
#[derive(Debug, Default, Parser)]
pub struct QuerierOptions {
    /// Actions you would like to take during the query. Available actions include:
    ///
    /// (1) `cg`: constructing the call graph(s) for the bytecode;
    /// (2) `dep`: constructing the dependency graph for the bytecode
    #[clap(long, required = true)]
    pub query_action: Option<String>,
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

    pub fn query(&self, bytecode_path: &Path) -> Result<String> {
        Ok(self.options.query_action.clone().unwrap())
    }

}
