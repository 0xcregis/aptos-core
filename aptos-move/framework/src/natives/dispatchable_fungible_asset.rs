use super::function_info::extract_function_info;
use aptos_gas_schedule::gas_params::natives::aptos_framework::DISPATCHABLE_FUNGIBLE_ASSET_DISPATCH_BASE;
// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0
use aptos_native_interface::{
    RawSafeNative, SafeNativeBuilder, SafeNativeContext, SafeNativeError, SafeNativeResult,
};
use move_vm_runtime::native_functions::NativeFunction;
use move_vm_types::{loaded_data::runtime_types::Type, values::Value};
use smallvec::SmallVec;
use std::collections::VecDeque;

/***************************************************************************************************
 * native fun dispatchable_withdraw / dispatchable_deposit / dispatchable_derived_balance / dispatchable_derived_supply
 *
 *   Directs control flow based on the last argument. We use the same native function implementation
 *   for all dispatching native.
 *   gas cost: a flat fee because we charged the loading of those modules previously.
 *
 **************************************************************************************************/
pub(crate) fn native_dispatch(
    context: &mut SafeNativeContext,
    ty_args: Vec<Type>,
    mut arguments: VecDeque<Value>,
) -> SafeNativeResult<SmallVec<[Value; 1]>> {
    let (module_name, func_name) = extract_function_info(&mut arguments)?;
    let index_manager = context
        .resolver
        .module_storage
        .runtime_environment()
        .struct_name_index_map();
    let function_idx =
        index_manager.function_idx(module_name.address(), &module_name.name, &func_name);
    let module_idx = function_idx.module_idx();

    // Check if the module is already properly charged in this transaction.
    let is_err = if context.get_feature_flags().is_account_abstraction_enabled() {
        !module_idx.is_special_addr()
            && !context
                .traversal_context()
                .visited
                .contains_key(&module_idx)
    } else {
        !context
            .traversal_context()
            .visited
            .contains_key(&module_idx)
    };
    if is_err {
        return Err(SafeNativeError::Abort { abort_code: 4 });
    }

    // Use Error to instruct the VM to perform a function call dispatch.
    Err(SafeNativeError::FunctionDispatch {
        cost: context.eval_gas(DISPATCHABLE_FUNGIBLE_ASSET_DISPATCH_BASE),
        idx: function_idx,
        ty_args,
        args: arguments.into_iter().collect(),
    })
}

/***************************************************************************************************
 * module
 *
 **************************************************************************************************/
pub fn make_all(
    builder: &SafeNativeBuilder,
) -> impl Iterator<Item = (String, NativeFunction)> + '_ {
    let natives = [
        ("dispatchable_withdraw", native_dispatch as RawSafeNative),
        ("dispatchable_deposit", native_dispatch),
        ("dispatchable_derived_balance", native_dispatch),
        ("dispatchable_derived_supply", native_dispatch),
    ];

    builder.make_named_natives(natives)
}
