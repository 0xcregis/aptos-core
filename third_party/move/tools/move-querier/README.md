---
id: move-querier
title: Move Querier
---

# Summary

The Move Query tool offers functions to build knowledge, such as call 
graph(s) and depdendency graph, about Move bytecode. It has two modes:

- **Integrated Mode**: This mode is integrated as a part of the `aptos move` toolchain. It can be invoked via `aptos move query [args]`

- **Standalone Mode**: TBD

# Integrated Mode

This mode is integrated into the `aptos move` toolchain in the same way as the `decompiler` and the `disassembler`. It will be automatically compiled when building the aptos-core project.

**Usage**: `aptos move query --query-action <QUERY_ACTION> <--package-path <PACKAGE_PATH>|--bytecode-path <BYTECODE_PATH>>`

> Available `UERY_ACTION` include:

> - `cg`: constructing the call graph(s) for the bytecode
> - `dep`: constructing the dependency graph for the bytecode

**Design**: It reuses the uniform interfaces from [bytecode.rs](../../../../crates/aptos/src/move_tool/bytecode.rs) to support command line parsing and redirect the execution to the `query()` function implemented in this tool (see [querier.rs](./src/querier.rs)). 

# Standalone Mode

TBD

# Design of `query`

TBD