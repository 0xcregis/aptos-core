// Copyright (c) The Aptos Labs
// Copyright (c) The Move Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Result};
use clap::Args;
use move_binary_format::{
    access::ScriptAccess, binary_views::BinaryIndexedView, file_format::{Bytecode, CodeUnit, CompiledScript, FunctionDefinitionIndex, FunctionHandle, FunctionHandleIndex, IdentifierIndex, SignatureToken, TableIndex}, CompiledModule
};
use petgraph::dot::{Dot, Config};
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use std::hash::Hash;

//Constants for result file extension
const CG_EXTENSION: &str = "mv.cg.dot";
const TYPE_EXTENSION: &str = "mv.type";
const UNKNOWN_EXTENSION: &str = "mv.unknown";

//Constants for bytecode type
const SCRIPT_CODE: &str = "script";
const MODULE_CODE: &str = "module";
const UNKNOWN_CODE: &str = "unknown";

//Constants for function signatures
const MAIN_FUNCTION: &str = "main";
const SCRIPT_MODULE: &str = "self";
const ENTRY_MODIFIER: &str = "entry";
const EMPTY: &str = "";

/// Holds the commands that we support while querying the bytecode.
#[derive(Copy, Clone, Debug, Args)]
#[group(id = "cmd", required = false, multiple = false)]
pub struct QuerierOptions {
    /// Dump the call graph(s) from bytecode, which should only be used together with `aptos move query`
    #[arg(long, group = "cmd")]
    dump_call_graph: bool,
    /// Check the type of the bytecode (`script`, `module`, or `unknown`), which should only be used together with `aptos move query`
    #[arg(long, group = "cmd")]
    check_bytecode_type: bool,
}

impl QuerierOptions {
    // check if any query command is provided
    pub fn has_any_true(&self) -> bool {
        self.dump_call_graph || self.check_bytecode_type
    }
    // get a proper extension for the query results
    pub fn extension(&self) -> &'static str {
        if self.dump_call_graph {
            return CG_EXTENSION;
        }
        if self.check_bytecode_type{
            return TYPE_EXTENSION;
        }
       UNKNOWN_EXTENSION
    }
}

/// Represents an instance of a querier. The querier now supports dumping call graphs and checking bytecode tyoe
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
        let module: CompiledModule;
        let script: CompiledScript;
        //Get a binary view of the bytecode
        let bytecode = if let Ok(bytecode) = CompiledScript::deserialize(&self.bytecode_bytes) {
            script = bytecode;
            BinaryIndexedView::Script(&script)
        } else if let Ok(bytecode) = CompiledModule::deserialize(&self.bytecode_bytes) {
            module = bytecode;
            BinaryIndexedView::Module(&module)
        } else {
            return Err(anyhow!(
                "Query Error: The bytecode cannot be resolved as either a module or a script"
            ));
        };

        if self.options.dump_call_graph {
            let cgbuider = CGBuilder::new(bytecode);
            return cgbuider.dump_call_graph();
        }

        if self.options.check_bytecode_type {
            let btchecker = BytecodeTypeChecker::new(bytecode);
            return btchecker.get_bytecode_type();
        }

        unreachable!("Not supported");

    }

}

pub struct BytecodeTypeChecker<'view> {
    bytecode: BinaryIndexedView<'view>,
}

impl<'view> BytecodeTypeChecker<'view>{

    pub fn new(bytecode: BinaryIndexedView<'view>) ->Self {
        Self {
            bytecode,
        }
    }

    pub fn get_bytecode_type(&self) -> Result<String> {
        match &self.bytecode {
            BinaryIndexedView::Script(_) => {
                Ok(String::from(SCRIPT_CODE))
            }
            BinaryIndexedView::Module(_) => {
                Ok(String::from(MODULE_CODE))
            }
            _ => {
                Ok(String::from(UNKNOWN_CODE))
            }
        }
    }
}

// node structure for call graphs
#[derive(Clone, Hash, Eq, PartialEq, PartialOrd, Ord)]
pub struct CGNode{
    func_sig: String,
}

impl std::fmt::Debug for CGNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print nothing here to avoid the default label.
        write!(f, "")
    }
}

pub struct QueryGraph<T: Eq + Clone + Hash>{
    nodes: HashMap<T, NodeIndex>,
    graph: DiGraph<T, ()>,
}

impl<T: Eq + Clone + Hash> QueryGraph<T> {
    pub fn new() -> Self {
       Self{
        nodes: HashMap::new(),
        graph: DiGraph::new(),
       }
    }

    fn add_node(&mut self, val: T) {
        if !self.nodes.contains_key(&val){
            let node_index = self.graph.add_node(val.clone());
            self.nodes.insert(val, node_index);
        }
    }

    fn add_edge(&mut self, src: T, dst: T) {
        if let (Some(src_index), Some(dst_index)) = (self.nodes.get(&src), self.nodes.get(&dst)) {
            self.graph.add_edge(*src_index, *dst_index, ());
        }
    }
}

pub struct CGBuilder<'view> {
    bytecode: BinaryIndexedView<'view>,
}

impl<'view> CGBuilder<'view>{
    pub fn new(bytecode: BinaryIndexedView<'view>) ->Self {
        Self {
            bytecode,
        }
    }
    fn format_function_sig(&self, entry_modifier: &str,
        native_modifier: &str, visibility_modifier: &str,
        module_name: &str, func_name: &str, ty_params: &str, params: &str,
        ret_type: &str) -> String {
            format!("{entry_modifier} {native_modifier} {visibility_modifier} {module_name}::{func_name}{ty_params}({params}){ret_type}")
    }

    // code mostly copied from the move disassembler code
    fn format_sig_token(&self, token: &SignatureToken) -> String {
        match token {
            SignatureToken::Bool => "bool".to_string(),
            SignatureToken::U8 => "u8".to_string(),
            SignatureToken::U16 => "u16".to_string(),
            SignatureToken::U32 => "u32".to_string(),
            SignatureToken::U64 => "u64".to_string(),
            SignatureToken::U128 => "u128".to_string(),
            SignatureToken::U256 => "u256".to_string(),
            SignatureToken::Address => "address".to_string(),
            SignatureToken::Signer => "signer".to_string(),
            SignatureToken::Vector(inner) => format!("vector<{}>", self.format_sig_token(inner)),
            SignatureToken::Struct(idx) => self.bytecode.identifier_at(self.bytecode.struct_handle_at(*idx).name).to_string(),
            SignatureToken::TypeParameter(idx) => format!("T{}", idx),
            SignatureToken::StructInstantiation(idx, type_args) => {
                let args: Vec<String> = type_args.iter().map(|token| self.format_sig_token(token)).collect();
                format!("struct_instantiation({:?}, <{}>)", idx, args.join(", "))
            },
            SignatureToken::Reference(sig_tok) => format!(
                "&{}",
                self.format_sig_token(&*sig_tok)
            ),
            _ => "unknown".to_string(),
        }
    }

    fn disassemble_instruction(&self,  instruction: &Bytecode) -> Option<FunctionHandleIndex> {
        match instruction {
            Bytecode::Call(method_idx) => Some(*method_idx),
            _ => None,
        }
    }

    fn get_child_functions(&self, code: Option<&CodeUnit>) -> Vec<FunctionHandleIndex>{
        match code {
            Some(code) => {
                code.code
                .iter()
                .filter_map(|inst| self.disassemble_instruction(inst)) // returns Option<FunctionHandleIndex>
                .collect::<Vec<FunctionHandleIndex>>()
            },
            None => Vec::<FunctionHandleIndex>::new(),
        }
    }

    fn dump_script_call_graph(&self, script: &CompiledScript) -> Result<QueryGraph<CGNode>> {
        let mut call_graph = QueryGraph::<CGNode>::new();

        // add the entry function node
        let entry_modifier = ENTRY_MODIFIER;
        let native_modifier: &str = EMPTY;
        let visibility_modifier = EMPTY;
        let module_name = SCRIPT_MODULE;
        let func_name = MAIN_FUNCTION;
        let ty_params = EMPTY;
        let params : Vec<String>= self.bytecode.signature_at(script.parameters).0.iter().map(|token|self.format_sig_token(token)).collect();
        let ret_type = EMPTY;
        let script_entry_node = CGNode{func_sig :
            self.format_function_sig(entry_modifier, native_modifier, visibility_modifier, module_name, func_name, ty_params, params.join(" ").as_str(), ret_type)};

        call_graph.add_node(script_entry_node.clone());

        let child_funcs = self.get_child_functions(Some(&script.code));

        child_funcs.iter()
        .for_each(|func_idx| {
            let function_handle = script.function_handle_at(*func_idx);
            let module_handle = script.module_handle_at(function_handle.module);

            let entry_modifier = EMPTY;
            let native_modifier: &str = EMPTY;
            let visibility_modifier = EMPTY;
            let module_id = script.module_id_for_handle(module_handle);
            let module_name = module_id.name.as_str();
            let func_name = script.identifier_at(function_handle.name).as_str();
            let ty_params = EMPTY;
            let params: Vec<String>= self.bytecode.signature_at(function_handle.parameters).0.iter().map(|token|self.format_sig_token(token)).collect();
            let ret_type: Vec<String>= self.bytecode.signature_at(function_handle.return_).0.iter().map(|token|self.format_sig_token(token)).collect();
            let child_node = CGNode{func_sig :
                self.format_function_sig(entry_modifier, native_modifier, visibility_modifier, module_name, func_name, ty_params, params.join(" ").as_str(), ret_type.join(" ").as_str())};
            call_graph.add_node(child_node.clone());
            call_graph.add_edge(script_entry_node.clone(), child_node.clone());
        });

        Ok(call_graph)
    }

    fn dump_module_call_graph(&self) -> QueryGraph<CGNode> {
        let call_graph = QueryGraph::<CGNode>::new();

        call_graph
    }

    fn dump_call_graph(&self) -> Result<String> {
       let call_graph: QueryGraph<CGNode> = match &self.bytecode {
            BinaryIndexedView::Script(script) => {
                self.dump_script_call_graph(*script)?
            }
            BinaryIndexedView::Module(_) => {
                self.dump_module_call_graph()
            }
        };

    let dot = Dot::with_attr_getters(
        &call_graph.graph,
        &[Config::EdgeNoLabel],
        &|_graph, _| "".to_string(),
        &|_graph, (_, node)| format!("label=\"{}\"", node.func_sig),
    );

        Ok(format!("{:?}", dot))
    }
}
