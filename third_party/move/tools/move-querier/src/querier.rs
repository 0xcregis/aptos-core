// Copyright (c) The Aptos Labs
// Copyright (c) The Move Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;

use std::path::Path;

/// Holds the actions that we support while querying the bytecode.
#[derive(Debug, Default)]
pub struct QuerierOptions {
    /// Actions you would like to take during the query. Available actions include:
    ///
    /// (1) `cg`: constructing the call graph(s) for the bytecode;
    /// (2) `dep`: constructing the dependency graph for the bytecode
    pub action: Option<String>,
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
