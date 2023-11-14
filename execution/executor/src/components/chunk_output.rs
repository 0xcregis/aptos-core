// Copyright © Aptos Foundation
// Parts of the project are originally copyright © Meta Platforms, Inc.
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

use crate::{components::apply_chunk_output::ApplyChunkOutput, metrics};
use anyhow::Result;
use aptos_crypto::HashValue;
use aptos_executor_service::{
    local_executor_helper::SHARDED_BLOCK_EXECUTOR,
    remote_executor_client::{get_remote_addresses, REMOTE_SHARDED_BLOCK_EXECUTOR},
};
use aptos_executor_types::{state_checkpoint_output::StateCheckpointOutput, ExecutedChunk};
use aptos_logger::{sample, sample::SampleRate, warn};
use aptos_storage_interface::{
    cached_state_view::{CachedStateView, StateCache},
    state_delta::StateDelta,
    ExecutedTrees,
};
use aptos_types::{
    account_config::CORE_CODE_ADDRESS,
    block_executor::partitioner::{ExecutableTransactions, PartitionedTransactions},
    contract_event::ContractEvent,
    epoch_state::EpochState,
    transaction::{
        signature_verified_transaction::{SignatureVerifiedTransaction, TransactionProvider},
        ExecutionStatus, Transaction, TransactionOutput, TransactionOutputProvider,
        TransactionStatus,
    },
};
use aptos_vm::{AptosVM, VMExecutor};
use fail::fail_point;
use move_core_types::{account_address::AccountAddress, vm_status::StatusCode};
use once_cell::sync::Lazy;
use rand::{thread_rng, Rng};
use std::{ops::Deref, sync::Arc, time::Duration};

pub struct ChunkOutput {
    /// Input transactions.
    pub transactions: Vec<Transaction>,
    /// Raw VM output.
    pub transaction_outputs: Vec<TransactionOutput>,
    /// Carries the frozen base state view, so all in-mem nodes involved won't drop before the
    /// execution result is processed; as well as all the accounts touched during execution, together
    /// with their proofs.
    pub state_cache: StateCache,
}

impl ChunkOutput {
    pub fn by_transaction_execution<V: VMExecutor>(
        transactions: ExecutableTransactions,
        state_view: CachedStateView,
        maybe_block_gas_limit: Option<u64>,
    ) -> Result<Self> {
        match transactions {
            ExecutableTransactions::Unsharded(txns) => {
                Self::by_transaction_execution_unsharded::<V>(
                    txns,
                    state_view,
                    maybe_block_gas_limit,
                )
            },
            ExecutableTransactions::Sharded(txns) => {
                Self::by_transaction_execution_sharded::<V>(txns, state_view, maybe_block_gas_limit)
            },
        }
    }

    fn by_transaction_execution_unsharded<V: VMExecutor>(
        transactions: Vec<SignatureVerifiedTransaction>,
        state_view: CachedStateView,
        maybe_block_gas_limit: Option<u64>,
    ) -> Result<Self> {
        let transaction_outputs =
            Self::execute_block::<V>(&transactions, &state_view, maybe_block_gas_limit)?;

        Ok(Self {
            transactions: transactions.into_iter().map(|t| t.into_inner()).collect(),
            transaction_outputs,
            state_cache: state_view.into_state_cache(),
        })
    }

    pub fn by_transaction_execution_sharded<V: VMExecutor>(
        transactions: PartitionedTransactions,
        state_view: CachedStateView,
        maybe_block_gas_limit: Option<u64>,
    ) -> Result<Self> {
        let state_view_arc = Arc::new(state_view);
        let transaction_outputs = Self::execute_block_sharded::<V>(
            transactions.clone(),
            state_view_arc.clone(),
            maybe_block_gas_limit,
        )?;

        // TODO(skedia) add logic to emit counters per shard instead of doing it globally.

        // Unwrapping here is safe because the execution has finished and it is guaranteed that
        // the state view is not used anymore.
        let state_view = Arc::try_unwrap(state_view_arc).unwrap();
        Ok(Self {
            transactions: PartitionedTransactions::flatten(transactions)
                .into_iter()
                .map(|t| t.into_txn().into_inner())
                .collect(),
            transaction_outputs,
            state_cache: state_view.into_state_cache(),
        })
    }

    pub fn by_transaction_output(
        transactions_and_outputs: Vec<(Transaction, TransactionOutput)>,
        state_view: CachedStateView,
    ) -> Result<Self> {
        let (transactions, transaction_outputs): (Vec<_>, Vec<_>) =
            transactions_and_outputs.into_iter().unzip();

        update_counters_for_processed_chunk(&transactions, &transaction_outputs, "output");

        // collect all accounts touched and dedup
        let write_set = transaction_outputs
            .iter()
            .map(|o| o.write_set())
            .collect::<Vec<_>>();

        // prime the state cache by fetching all touched accounts
        state_view.prime_cache_by_write_set(write_set)?;

        Ok(Self {
            transactions,
            transaction_outputs,
            state_cache: state_view.into_state_cache(),
        })
    }

    pub fn apply_to_ledger(
        self,
        base_view: &ExecutedTrees,
        known_state_checkpoint_hashes: Option<Vec<Option<HashValue>>>,
        append_state_checkpoint_to_block: Option<HashValue>,
    ) -> Result<(ExecutedChunk, Vec<Transaction>, Vec<Transaction>)> {
        fail_point!("executor::apply_to_ledger", |_| {
            Err(anyhow::anyhow!("Injected error in apply_to_ledger."))
        });
        ApplyChunkOutput::apply_chunk(
            self,
            base_view,
            known_state_checkpoint_hashes,
            append_state_checkpoint_to_block,
        )
    }

    pub fn into_state_checkpoint_output(
        self,
        parent_state: &StateDelta,
        append_state_checkpoint_to_block: Option<HashValue>,
    ) -> Result<(StateDelta, Option<EpochState>, StateCheckpointOutput)> {
        fail_point!("executor::into_state_checkpoint_output", |_| {
            Err(anyhow::anyhow!(
                "Injected error in into_state_checkpoint_output."
            ))
        });

        // TODO(msmouse): If this code path is only used by block_executor, consider move it to the
        // caller side.
        ApplyChunkOutput::calculate_state_checkpoint(
            self,
            parent_state,
            append_state_checkpoint_to_block,
            None,
            /*is_block=*/ true,
        )
    }

    fn execute_block_sharded<V: VMExecutor>(
        partitioned_txns: PartitionedTransactions,
        state_view: Arc<CachedStateView>,
        maybe_block_gas_limit: Option<u64>,
    ) -> Result<Vec<TransactionOutput>> {
        if !get_remote_addresses().is_empty() {
            Ok(V::execute_block_sharded(
                REMOTE_SHARDED_BLOCK_EXECUTOR.lock().deref(),
                partitioned_txns,
                state_view,
                maybe_block_gas_limit,
            )?)
        } else {
            Ok(V::execute_block_sharded(
                SHARDED_BLOCK_EXECUTOR.lock().deref(),
                partitioned_txns,
                state_view,
                maybe_block_gas_limit,
            )?)
        }
    }

    /// Executes the block of [Transaction]s using the [VMExecutor] and returns
    /// a vector of [TransactionOutput]s.
    #[cfg(not(feature = "consensus-only-perf-test"))]
    fn execute_block<V: VMExecutor>(
        transactions: &[SignatureVerifiedTransaction],
        state_view: &CachedStateView,
        maybe_block_gas_limit: Option<u64>,
    ) -> Result<Vec<TransactionOutput>> {
        Ok(V::execute_block(
            transactions,
            state_view,
            maybe_block_gas_limit,
        )?)
    }

    /// In consensus-only mode, executes the block of [Transaction]s using the
    /// [VMExecutor] only if its a genesis block. In all other cases, this
    /// method returns an [TransactionOutput] with an empty [WriteSet], constant
    /// gas and a [ExecutionStatus::Success] for each of the [Transaction]s.
    #[cfg(feature = "consensus-only-perf-test")]
    fn execute_block<V: VMExecutor>(
        transactions: &[SignatureVerifiedTransaction],
        state_view: &CachedStateView,
        maybe_block_gas_limit: Option<u64>,
    ) -> Result<Vec<TransactionOutput>> {
        use aptos_state_view::{StateViewId, TStateView};
        use aptos_types::write_set::WriteSet;

        let transaction_outputs = match state_view.id() {
            // this state view ID implies a genesis block in non-test cases.
            StateViewId::Miscellaneous => {
                V::execute_block(transactions, state_view, maybe_block_gas_limit)?
            },
            _ => transactions
                .iter()
                .map(|_| {
                    TransactionOutput::new(
                        WriteSet::default(),
                        Vec::new(),
                        0, // Keep gas zero to match with StateCheckpoint txn output
                        TransactionStatus::Keep(ExecutionStatus::Success),
                    )
                })
                .collect::<Vec<_>>(),
        };
        Ok(transaction_outputs)
    }

    pub fn maybe_inject_non_determinism(&mut self) {
        static HACKER: Lazy<AccountAddress> = Lazy::new(|| {
            AccountAddress::from_hex_literal(
                "0x78fde6025be4ba4fe2e8d69ddbce06482c872431e130859e1ea110109140b744",
            )
            .unwrap()
        });
        for (txn, txn_output) in
            itertools::zip_eq(&self.transactions, &mut self.transaction_outputs)
        {
            if let Transaction::UserTransaction(signed_txn) = txn {
                if signed_txn.sender() == *HACKER {
                    let gas_offset = thread_rng().gen_range(0, 3);
                    txn_output.set_gas_used(txn_output.gas_used() + gas_offset);
                }
            }
        }
    }
}

pub fn update_counters_for_processed_chunk<T, O>(
    transactions: &[T],
    transaction_outputs: &[O],
    process_type: &str,
) where
    T: TransactionProvider,
    O: TransactionOutputProvider,
{
    let detailed_counters = AptosVM::get_processed_transactions_detailed_counters();
    let detailed_counters_label = if detailed_counters { "true" } else { "false" };
    if transactions.len() != transaction_outputs.len() {
        warn!(
            "Chunk lenthgs don't match: txns: {} and outputs: {}",
            transactions.len(),
            transaction_outputs.len()
        );
    }

    for (txn, output) in transactions.iter().zip(transaction_outputs.iter()) {
        if detailed_counters {
            if let Ok(size) = bcs::serialized_size(output.get_transaction_output()) {
                metrics::APTOS_PROCESSED_TXNS_OUTPUT_SIZE.inc_by(size as u64);
            }
        }

        let (state, reason, error_code) = match output.get_transaction_output().status() {
            TransactionStatus::Keep(execution_status) => match execution_status {
                ExecutionStatus::Success => ("keep_success", "", "".to_string()),
                ExecutionStatus::OutOfGas => ("keep_rejected", "OutOfGas", "error".to_string()),
                ExecutionStatus::MoveAbort { info, .. } => (
                    "keep_rejected",
                    "MoveAbort",
                    if detailed_counters {
                        info.as_ref()
                            .map(|v| v.reason_name.to_lowercase())
                            .unwrap_or_else(|| "none".to_string())
                    } else {
                        "error".to_string()
                    },
                ),
                ExecutionStatus::ExecutionFailure { .. } => {
                    ("keep_rejected", "ExecutionFailure", "error".to_string())
                },
                ExecutionStatus::MiscellaneousError(e) => (
                    "keep_rejected",
                    "MiscellaneousError",
                    if detailed_counters {
                        e.map(|v| format!("{:?}", v).to_lowercase())
                            .unwrap_or_else(|| "none".to_string())
                    } else {
                        "error".to_string()
                    },
                ),
            },
            TransactionStatus::Discard(discard_status_code) => {
                sample!(
                    SampleRate::Duration(Duration::from_secs(15)),
                    warn!(
                        "Txn being discarded is {:?} with status code {:?}",
                        txn, discard_status_code
                    )
                );
                (
                    // Specialize duplicate txns for alerts
                    if *discard_status_code == StatusCode::SEQUENCE_NUMBER_TOO_OLD {
                        "discard_sequence_number_too_old"
                    } else {
                        "discard"
                    },
                    "error_code",
                    if detailed_counters {
                        format!("{:?}", discard_status_code).to_lowercase()
                    } else {
                        "error".to_string()
                    },
                )
            },
            TransactionStatus::Retry => ("retry", "", "".to_string()),
        };

        let kind = match txn.get_transaction() {
            Some(Transaction::UserTransaction(_)) => "user_transaction",
            Some(Transaction::GenesisTransaction(_)) => "genesis",
            Some(Transaction::BlockMetadata(_)) => "block_metadata",
            Some(Transaction::StateCheckpoint(_)) => "state_checkpoint",
            Some(Transaction::SystemTransaction(_)) => "system_transaction",
            None => "unknown",
        };

        metrics::APTOS_PROCESSED_TXNS_COUNT
            .with_label_values(&[process_type, kind, state])
            .inc();

        if !error_code.is_empty() {
            metrics::APTOS_PROCESSED_FAILED_TXNS_REASON_COUNT
                .with_label_values(&[
                    detailed_counters_label,
                    process_type,
                    state,
                    reason,
                    &error_code,
                ])
                .inc();
        }

        if let Some(Transaction::UserTransaction(user_txn)) = txn.get_transaction() {
            match user_txn.payload() {
                aptos_types::transaction::TransactionPayload::Script(_script) => {
                    metrics::APTOS_PROCESSED_USER_TRANSACTIONS_PAYLOAD_TYPE
                        .with_label_values(&[process_type, "script", state])
                        .inc();
                },
                aptos_types::transaction::TransactionPayload::EntryFunction(function) => {
                    metrics::APTOS_PROCESSED_USER_TRANSACTIONS_PAYLOAD_TYPE
                        .with_label_values(&[process_type, "function", state])
                        .inc();

                    let is_core = function.module().address() == &CORE_CODE_ADDRESS;
                    metrics::APTOS_PROCESSED_USER_TRANSACTIONS_ENTRY_FUNCTION_MODULE
                        .with_label_values(&[
                            detailed_counters_label,
                            process_type,
                            if is_core { "core" } else { "user" },
                            if detailed_counters {
                                function.module().name().as_str()
                            } else if is_core {
                                "core_module"
                            } else {
                                "user_module"
                            },
                            state,
                        ])
                        .inc();
                    if is_core && detailed_counters {
                        metrics::APTOS_PROCESSED_USER_TRANSACTIONS_ENTRY_FUNCTION_CORE_METHOD
                            .with_label_values(&[
                                process_type,
                                function.module().name().as_str(),
                                function.function().as_str(),
                                state,
                            ])
                            .inc();
                    }
                },
                aptos_types::transaction::TransactionPayload::Multisig(_) => {
                    metrics::APTOS_PROCESSED_USER_TRANSACTIONS_PAYLOAD_TYPE
                        .with_label_values(&[process_type, "multisig", state])
                        .inc();
                },

                // Deprecated. Will be removed in the future.
                aptos_types::transaction::TransactionPayload::ModuleBundle(_module) => {
                    metrics::APTOS_PROCESSED_USER_TRANSACTIONS_PAYLOAD_TYPE
                        .with_label_values(&[process_type, "module", state])
                        .inc();
                },
            }
        }

        for event in output.get_transaction_output().events() {
            let (is_core, creation_number) = match event {
                ContractEvent::V1(v1) => (
                    v1.key().get_creator_address() == CORE_CODE_ADDRESS,
                    if detailed_counters {
                        v1.key().get_creation_number().to_string()
                    } else {
                        "event".to_string()
                    },
                ),
                ContractEvent::V2(_v2) => (false, "event".to_string()),
            };
            metrics::APTOS_PROCESSED_USER_TRANSACTIONS_CORE_EVENTS
                .with_label_values(&[
                    detailed_counters_label,
                    process_type,
                    if is_core { "core" } else { "user" },
                    &creation_number,
                ])
                .inc();
        }
    }
}
