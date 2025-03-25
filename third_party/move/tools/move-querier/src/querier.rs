// Copyright (c) The Aptos Labs
// Copyright (c) The Move Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Error, Result};
use clap::Args;
use move_binary_format::{
    binary_views::BinaryIndexedView,
    file_format::{CompiledScript, FunctionDefinitionIndex, FunctionHandle, FunctionHandleIndex, TableIndex,},
    CompiledModule,
};
use petgraph::graphmap::DiGraphMap;

const CG_EXTENSION: &str = "mv.cg.dot";
const DEP_EXTENSION: &str = "mv.dep.dot";
const UNKNOWN_EXTENSION: &str = "mv.unknown";

/// Holds the commands that we support while querying the bytecode.
#[derive(Copy, Clone, Debug, Args)]
#[group(id = "cmd", required = false, multiple = false)]
pub struct QuerierOptions {
    /// Dump the call graph(s) from bytecode, which should only be used together with `aptos move query`
    #[arg(long, group = "cmd")]
    dump_call_graph: bool,

    /// Dump the dependency graph(s) from bytecode, which should only be used together with `aptos move query`
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

enum BytecodeType {
    Module(CompiledModule),
    Script(CompiledScript),
}

/// Represents an instance of a querier. The querier can ...
pub struct Querier {
    options: QuerierOptions,
    bytecode_bytes: Vec<u8>,
    bytecode_type: Option<BytecodeType>,
    call_graph: DiGraphMap<FunctionHandleIndex, ()>,
}


impl Querier {
    /// Creates a new querier.
    pub fn new(options: QuerierOptions, bytecode_bytes: Vec<u8>) -> Self {
        Self {
            options,
            bytecode_bytes,
            bytecode_type: None,
            call_graph: DiGraphMap::new(),
        }
    }

    ///Main interface to run the query actions
    pub fn query(&mut self) -> Result<String> {

        //Determine the bytecode type
        self.bytecode_type = if let Ok(script) = CompiledScript::deserialize(&self.bytecode_bytes) {
            println!("This is a script");
            Some(BytecodeType::Script(script))
        } else if let Ok(module) = CompiledModule::deserialize(&self.bytecode_bytes) {
            println!("This is a module");
            Some(BytecodeType::Module(module))
        } else {
            None
        };

        // Create a binary indexed view for the bytecode bytes
        let bytecode = match &self.bytecode_type {
            Some(BytecodeType::Module(module)) => BinaryIndexedView::Module(module),
            Some(BytecodeType::Script(script)) => BinaryIndexedView::Script(script),
            _ => return Err(Error::msg("Failed to deserialize bytecode as either a module or script")),
        };
        if self.options.dump_call_graph {
            println!("dump_call_graph is called");
            return self.dump_call_graph(bytecode);
        }
        Ok("cfg".to_string())
    }

    fn dump_call_graph(&self, bytecode: BinaryIndexedView) -> Result<String> {

        let function_defs: Vec<String> = match bytecode {

            BinaryIndexedView::Script(script) => {

            println!("Script fun size: {}", script.function_handles.len());

            script.function_handles.iter()
            .enumerate()
            .map(|(index, function_handle)| {
                    Ok(bytecode.identifier_at(function_handle.name).as_str().to_string() + "Hello")
                })
                .collect::<Result<Vec<String>>>()?},

            BinaryIndexedView::Module(module) => {
                println!("Module fun size: {}", module.function_defs.len());
                (0..module.function_defs.len())
                .map(|i| {
                    let function_definition_index = FunctionDefinitionIndex(i as TableIndex);
                    let function_definition = bytecode.function_def_at(function_definition_index);

                    let function_handle = bytecode.function_handle_at(function_definition.unwrap().function);
                    Ok(bytecode.identifier_at(function_handle.name).as_str().to_string())
                })
                .collect::<Result<Vec<String>>>()?},
        };
        Ok(function_defs.join(" "))
    }
}
