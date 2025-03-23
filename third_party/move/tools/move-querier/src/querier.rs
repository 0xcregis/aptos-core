// Copyright (c) The Aptos Labs
// Copyright (c) The Move Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Args;
use move_bytecode_source_map::mapping::SourceMapping;

use std::path::Path;
use std::collections::HashMap;

const CG_EXTENSION: &str = "mv.cg";
const DEP_EXTENSION: &str = "mv.dep";
const UNKNOWN_EXTENSION: &str = "mv.unknown";

/// Holds the commands that we support while querying the bytecode.
#[derive(Copy, Clone, Debug, Args)]
#[group(id = "cmd", required = false, multiple = false)]
pub struct QuerierOptions {

    /// Dump the call graph(s) from bytecode, which should only be used together with `aptos move query`
    #[arg(long, group = "cmd")]
    dump_call_graph: bool,

    /// Dump the call graph(s) from bytecode, which should only be used together with `aptos move query`
    #[arg(long, group = "cmd")]
    dump_dep_graph: bool,
}

impl QuerierOptions {

    //function to check if any query command has been provided
    pub fn has_any_true(&self) -> bool {
        self.dump_call_graph || self.dump_dep_graph
    }

    //function to get a proper extension for the query results
    pub fn extension(&self) -> &'static str {
        if self.dump_call_graph {
            return CG_EXTENSION;
        }

        if self.dump_dep_graph{
            return DEP_EXTENSION;
        }
       UNKNOWN_EXTENSION
    }
}




/// Represents an instance of a querier. The querier can ...
pub struct Querier {
    options: QuerierOptions,
    bytecode_bytes: Vec<u8>,
}


impl Querier {
    /// Creates a new querier.
    pub fn new(options: QuerierOptions, bytecode_bytes: Vec<u8>) -> Self {

        Self {
            options,
            bytecode_bytes,
        }
    }

    ///Main interface to run the query actions
    pub fn query(&self) -> Result<String> {
        Ok("cfg".to_string())
    }

}
