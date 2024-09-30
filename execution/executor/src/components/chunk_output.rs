// Copyright © Aptos Foundation
// Parts of the project are originally copyright © Meta Platforms, Inc.
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

use crate::{components::apply_chunk_output::ApplyChunkOutput, metrics};
use anyhow::{anyhow, Result};
use aptos_crypto::HashValue;
use aptos_executor_service::{
    local_executor_helper::SHARDED_BLOCK_EXECUTOR,
    remote_executor_client::{get_remote_addresses, REMOTE_SHARDED_BLOCK_EXECUTOR},
};
use aptos_executor_types::{state_checkpoint_output::StateCheckpointOutput, ExecutedChunk, ParsedTransactionOutput};
use aptos_logger::{error, sample, sample::SampleRate, warn};
use aptos_storage_interface::{
    cached_state_view::{CachedStateView, StateCache},
    state_delta::StateDelta,
    ExecutedTrees,
};
use aptos_types::{
    account_config::CORE_CODE_ADDRESS,
    block_executor::{
        config::BlockExecutorConfigFromOnchain,
        partitioner::{ExecutableTransactions, PartitionedTransactions},
    },
    contract_event::ContractEvent,
    epoch_state::EpochState,
    transaction::{
        authenticator::AccountAuthenticator,
        block_epilogue::BlockEndInfo,
        signature_verified_transaction::{SignatureVerifiedTransaction, TransactionProvider},
        BlockOutput, ExecutionStatus, Transaction, TransactionOutput, TransactionOutputProvider,
        TransactionStatus,
    },
};
use aptos_vm::{AptosVM, VMExecutor};
use fail::fail_point;
use move_core_types::vm_status::StatusCode;
use std::{ops::Deref, sync::Arc, time::Duration};
use std::iter::repeat;
use aptos_executor_types::parsed_transaction_output::TransactionsWithParsedOutput;
use aptos_executor_types::state_checkpoint_output::TransactionsByStatus;
use aptos_metrics_core::TimerHelper;
use aptos_storage_interface::cached_state_view::ShardedStateCache;
use aptos_types::on_chain_config::{ConfigurationResource, ValidatorSet};
use aptos_types::state_store::ShardedStateUpdates;
use aptos_types::transaction::{BlockEpiloguePayload, TransactionAuxiliaryData};
use aptos_types::write_set::WriteSet;
use crate::metrics::{EXECUTOR_ERRORS, OTHER_TIMERS};

pub struct RawChunkOutputParser {
    /// Input transactions.
    pub transactions: Vec<Transaction>,
    /// Raw VM output.
    pub transaction_outputs: Vec<TransactionOutput>,
    /// Carries the frozen base state view, so all in-mem nodes involved won't drop before the
    /// execution result is processed; as well as all the accounts touched during execution, together
    /// with their proofs.
    pub state_cache: StateCache,
    /// BlockEndInfo outputed by the VM
    pub block_end_info: Option<BlockEndInfo>,
}

impl RawChunkOutputParser {
    fn parse(
        self,
        append_state_checkpoint_to_block: Option<HashValue>,
    ) -> Result<ChunkOutput> {
        let Self {
            mut transactions,
            transaction_outputs,
            state_cache,
            block_end_info,
        } = self;

        // Parse all outputs.
        let mut transaction_outputs: Vec<ParsedTransactionOutput> = {
            let _timer = OTHER_TIMERS.timer_with(&["parse_output"]);
            transaction_outputs.into_iter().map(Into::into).collect()
        };

        // Isolate retries.
        let to_retry = Self::extract_retries(&mut transactions, &mut transaction_outputs);

        // Collect all statuses.
        let statuses_for_input_txns = {
            let keeps_and_discards = transaction_outputs.iter().map(|t| t.status()).cloned();
            // Forcibly overwriting statuses for retries, since VM can output otherwise.
            let retries = repeat(TransactionStatus::Retry).take(to_retry.len());
            keeps_and_discards.chain(retries).collect()
        };

        // Isolate discards.
        let to_discard = Self::extract_discards(&mut transactions, &mut transaction_outputs);

        // The rest is to be committed, attach block epilogue as needed and optionally get next EpochState.
        let to_commit = TransactionsWithParsedOutput::new(transactions, transaction_outputs);
        let to_commit = Self::maybe_add_block_epilogue(
            to_commit,
            block_end_info.as_ref(),
            append_state_checkpoint_to_block,
        );
        let next_epoch_state = if to_commit.ends_epoch() {
            Self::get_epoch_state(&state_cache.base, &state_cache.updates).ok()
        };

        Ok(ChunkOutput {
            statuses_for_input_txns,
            to_commit,
            to_discard,
            to_retry,
            state_cache,
            block_end_info,
            next_epoch_state,
        })
    }

    fn extract_discards(mut transactions: &mut Vec<Transaction>, mut transaction_outputs: &mut Vec<ParsedTransactionOutput>) -> TransactionsWithParsedOutput {
        let to_discard = {
            let mut res = TransactionsWithParsedOutput::new_empty();
            for idx in 0..transactions.len() {
                if transaction_outputs[idx].status().is_discarded() {
                    res.push(transactions[idx].clone(), transaction_outputs[idx].clone());
                } else if !res.is_empty() {
                    transactions[idx - res.len()] = transactions[idx].clone();
                    transaction_outputs[idx - res.len()] = transaction_outputs[idx].clone();
                }
            }
            if !res.is_empty() {
                let remaining = transactions.len() - res.len();
                transactions.truncate(remaining);
                transaction_outputs.truncate(remaining);
            }
            res
        };

        // Sanity check transactions with the Discard status:
        to_discard.iter().for_each(|(t, o)| {
            // In case a new status other than Retry, Keep and Discard is added:
            if !matches!(o.status(), TransactionStatus::Discard(_)) {
                error!("Status other than Retry, Keep or Discard; Transaction discarded.");
            }
            // VM shouldn't have output anything for discarded transactions, log if it did.
            if !o.write_set().is_empty() || !o.events().is_empty() {
                error!(
                    "Discarded transaction has non-empty write set or events. \
                        Transaction: {:?}. Status: {:?}.",
                    t,
                    o.status(),
                );
                EXECUTOR_ERRORS.inc();
            }
        });

        to_discard
    }

    fn extract_retries(
        mut transactions: &mut Vec<Transaction>,
        mut transaction_outputs: &mut Vec<ParsedTransactionOutput>
    ) -> TransactionsWithParsedOutput {
        // N.B. off-by-1 intentionally, for exclusive index
        let new_epoch_marker = transaction_outputs
            .iter()
            .position(|o| o.is_reconfig())
            .map(|idx| idx + 1);

        let block_gas_limit_marker = transaction_outputs
            .iter()
            .position(|o| matches!(o.status(), TransactionStatus::Retry));

        // Transactions after the epoch ending txn are all to be retried.
        // Transactions after the txn that exceeded per-block gas limit are also to be retried.
        if let Some(pos) = new_epoch_marker {
            TransactionsWithParsedOutput::new(
                transactions.drain(pos..).collect(),
                transaction_outputs.drain(pos..).collect(),
            )
        } else if let Some(pos) = block_gas_limit_marker {
            TransactionsWithParsedOutput::new(
                transactions.drain(pos..).collect(),
                transaction_outputs.drain(pos..).collect(),
            )
        } else {
            TransactionsWithParsedOutput::new_empty()
        }
    }

    fn maybe_add_block_epilogue(
        mut to_commit: TransactionsWithParsedOutput,
        block_end_info: Option<&BlockEndInfo>,
        append_state_checkpoint_to_block: Option<HashValue>,
    ) -> TransactionsWithParsedOutput {
        if !to_commit.ends_epoch() {
            // Append the StateCheckpoint transaction to the end
            if let Some(block_id) = append_state_checkpoint_to_block {
                let state_checkpoint_txn = block_end_info.cloned().map_or(
                    Transaction::StateCheckpoint(block_id),
                    |block_end_info| {
                        Transaction::BlockEpilogue(BlockEpiloguePayload::V0 {
                            block_id,
                            block_end_info,
                        })
                    },
                );
                let state_checkpoint_txn_output: ParsedTransactionOutput =
                    Into::into(TransactionOutput::new(
                        WriteSet::default(),
                        Vec::new(),
                        0,
                        TransactionStatus::Keep(ExecutionStatus::Success),
                        TransactionAuxiliaryData::default(),
                    ));
                to_commit.push(state_checkpoint_txn, state_checkpoint_txn_output);
            }
        }; // else: not adding block epilogue at epoch ending.


        to_commit
    }

    fn get_epoch_state(
        base: &ShardedStateCache,
        updates: &ShardedStateUpdates,
    ) -> Result<EpochState> {

        let state_cache_view = StateCacheView::new(base, updates);
        let validator_set = ValidatorSet::fetch_config(&state_cache_view)
            .ok_or_else(|| anyhow!("ValidatorSet not touched on epoch change"))?;
        let configuration = ConfigurationResource::fetch_config(&state_cache_view)
            .ok_or_else(|| anyhow!("Configuration resource not touched on epoch change"))?;

        Ok(EpochState {
            epoch: configuration.epoch(),
            verifier: (&validator_set).into(),
        })
    }
}

pub struct ChunkOutput {
    // Statuses of the input transactions, in the same order as the input transactions.
    // Contains BlockMetadata/Validator transactions,
    // but doesn't contain StateCheckpoint/BlockEpilogue, as those get added during execution
    statuses_for_input_txns: Vec<TransactionStatus>,
    // List of all transactions to be committed, including StateCheckpoint/BlockEpilogue if needed.
    to_commit: TransactionsWithParsedOutput,
    to_discard: TransactionsWithParsedOutput,
    to_retry: TransactionsWithParsedOutput,

    /// Carries the frozen base state view, so all in-mem nodes involved won't drop before the
    /// execution result is processed; as well as all the accounts touched during execution, together
    /// with their proofs.
    pub state_cache: StateCache,
    /// Optional StateCheckpoint payload
    pub block_end_info: Option<BlockEndInfo>,
    /// Optional EpochState payload.
    /// Only present if the block is the last block of an epoch, and is parsed output of the
    /// state cache.
    pub next_epoch_state: Option<EpochState>,
}

impl ChunkOutput {
    pub fn by_transaction_execution<V: VMExecutor>(
        transactions: ExecutableTransactions,
        state_view: CachedStateView,
        onchain_config: BlockExecutorConfigFromOnchain,
        append_state_checkpoint_to_block: Option<HashValue>,
    ) -> Result<Self> {
        match transactions {
            ExecutableTransactions::Unsharded(txns) => {
                Self::by_transaction_execution_unsharded::<V>(txns, state_view, onchain_config, append_state_checkpoint_to_block)
            },
            ExecutableTransactions::Sharded(txns) => {
                Self::by_transaction_execution_sharded::<V>(txns, state_view, onchain_config, append_state_checkpoint_to_block)
            },
        }
    }

    fn by_transaction_execution_unsharded<V: VMExecutor>(
        transactions: Vec<SignatureVerifiedTransaction>,
        state_view: CachedStateView,
        onchain_config: BlockExecutorConfigFromOnchain,
        append_state_checkpoint_to_block: Option<HashValue>,
    ) -> Result<Self> {
        let block_output = Self::execute_block::<V>(&transactions, &state_view, onchain_config)?;
        let (transaction_outputs, block_end_info) = block_output.into_inner();

        RawChunkOutputParser {
            transactions: transactions.into_iter().map(|t| t.into_inner()).collect(),
            transaction_outputs,
            state_cache,
            block_end_info,
        }.parse(
            append_state_checkpoint_to_block,
        )
    }

    pub fn by_transaction_execution_sharded<V: VMExecutor>(
        transactions: PartitionedTransactions,
        state_view: CachedStateView,
        onchain_config: BlockExecutorConfigFromOnchain,
        append_state_checkpoint_to_block: Option<HashValue>,
    ) -> Result<Self> {
        let state_view_arc = Arc::new(state_view);
        let transaction_outputs = Self::execute_block_sharded::<V>(
            transactions.clone(),
            state_view_arc.clone(),
            onchain_config,
        )?;

        // TODO(skedia) add logic to emit counters per shard instead of doing it globally.

        // Unwrapping here is safe because the execution has finished and it is guaranteed that
        // the state view is not used anymore.
        let state_view = Arc::try_unwrap(state_view_arc).unwrap();
        RawChunkOutputParser {
            transactions: PartitionedTransactions::flatten(transactions)
                .into_iter()
                .map(|t| t.into_txn().into_inner())
                .collect(),
            transaction_outputs,
            state_cache: state_view.into_state_cache(),
            block_end_info: None,
        }.parse(
            append_state_checkpoint_to_block
        )
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

        RawChunkOutputParser {
            transactions,
            transaction_outputs,
            state_cache: state_view.into_state_cache(),
            block_end_info: None,
        }.parse(None)
    }

    pub fn apply_to_ledger(
        self,
        base_view: &ExecutedTrees,
        known_state_checkpoint_hashes: Option<Vec<Option<HashValue>>>,
    ) -> Result<(ExecutedChunk, Vec<Transaction>, Vec<Transaction>)> {
        fail_point!("executor::apply_to_ledger", |_| {
            Err(anyhow::anyhow!("Injected error in apply_to_ledger."))
        });
        ApplyChunkOutput::apply_chunk(self, base_view, known_state_checkpoint_hashes)
    }

    pub fn into_state_checkpoint_output(
        self,
        parent_state: &StateDelta,
        block_id: HashValue,
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
            Some(block_id),
            None,
            /*is_block=*/ true,
        )
    }

    fn execute_block_sharded<V: VMExecutor>(
        partitioned_txns: PartitionedTransactions,
        state_view: Arc<CachedStateView>,
        onchain_config: BlockExecutorConfigFromOnchain,
    ) -> Result<Vec<TransactionOutput>> {
        if !get_remote_addresses().is_empty() {
            Ok(V::execute_block_sharded(
                REMOTE_SHARDED_BLOCK_EXECUTOR.lock().deref(),
                partitioned_txns,
                state_view,
                onchain_config,
            )?)
        } else {
            Ok(V::execute_block_sharded(
                SHARDED_BLOCK_EXECUTOR.lock().deref(),
                partitioned_txns,
                state_view,
                onchain_config,
            )?)
        }
    }

    /// Executes the block of [Transaction]s using the [VMExecutor] and returns
    /// a vector of [TransactionOutput]s.
    #[cfg(not(feature = "consensus-only-perf-test"))]
    fn execute_block<V: VMExecutor>(
        transactions: &[SignatureVerifiedTransaction],
        state_view: &CachedStateView,
        onchain_config: BlockExecutorConfigFromOnchain,
    ) -> Result<BlockOutput<TransactionOutput>> {
        Ok(V::execute_block(transactions, state_view, onchain_config)?)
    }

    /// In consensus-only mode, executes the block of [Transaction]s using the
    /// [VMExecutor] only if its a genesis block. In all other cases, this
    /// method returns an [TransactionOutput] with an empty [WriteSet], constant
    /// gas and a [ExecutionStatus::Success] for each of the [Transaction]s.
    #[cfg(feature = "consensus-only-perf-test")]
    fn execute_block<V: VMExecutor>(
        transactions: &[SignatureVerifiedTransaction],
        state_view: &CachedStateView,
        onchain_config: BlockExecutorConfigFromOnchain,
    ) -> Result<BlockOutput<TransactionOutput>> {
        use aptos_types::{
            state_store::{StateViewId, TStateView},
            transaction::TransactionAuxiliaryData,
            write_set::WriteSet,
        };

        let transaction_outputs = match state_view.id() {
            // this state view ID implies a genesis block in non-test cases.
            StateViewId::Miscellaneous => {
                V::execute_block(transactions, state_view, onchain_config)?
            },
            _ => BlockOutput::new(
                transactions
                    .iter()
                    .map(|_| {
                        TransactionOutput::new(
                            WriteSet::default(),
                            Vec::new(),
                            0, // Keep gas zero to match with StateCheckpoint txn output
                            TransactionStatus::Keep(ExecutionStatus::Success),
                            TransactionAuxiliaryData::None,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        };
        Ok(transaction_outputs)
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
                metrics::PROCESSED_TXNS_OUTPUT_SIZE
                    .with_label_values(&[process_type])
                    .observe(size as f64);
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
                (
                    // Specialize duplicate txns for alerts
                    if *discard_status_code == StatusCode::SEQUENCE_NUMBER_TOO_OLD {
                        "discard_sequence_number_too_old"
                    } else if *discard_status_code == StatusCode::SEQUENCE_NUMBER_TOO_NEW {
                        "discard_sequence_number_too_new"
                    } else if *discard_status_code == StatusCode::TRANSACTION_EXPIRED {
                        "discard_transaction_expired"
                    } else {
                        // Only log if it is an interesting discard
                        sample!(
                            SampleRate::Duration(Duration::from_secs(15)),
                            warn!(
                                "[sampled] Txn being discarded is {:?} with status code {:?}",
                                txn, discard_status_code
                            )
                        );
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
            Some(Transaction::BlockMetadataExt(_)) => "block_metadata_ext",
            Some(Transaction::StateCheckpoint(_)) => "state_checkpoint",
            Some(Transaction::BlockEpilogue(_)) => "block_epilogue",
            Some(Transaction::ValidatorTransaction(_)) => "validator_transaction",
            None => "unknown",
        };

        metrics::PROCESSED_TXNS_COUNT
            .with_label_values(&[process_type, kind, state])
            .inc();

        if !error_code.is_empty() {
            metrics::PROCESSED_FAILED_TXNS_REASON_COUNT
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
            if detailed_counters {
                let mut signature_count = 0;
                let account_authenticators = user_txn.authenticator_ref().all_signers();
                for account_authenticator in account_authenticators {
                    match account_authenticator {
                        AccountAuthenticator::Ed25519 { .. } => {
                            signature_count += 1;
                            metrics::PROCESSED_TXNS_AUTHENTICATOR
                                .with_label_values(&[process_type, "Ed25519"])
                                .inc();
                        },
                        AccountAuthenticator::MultiEd25519 { signature, .. } => {
                            let count = signature.signatures().len();
                            signature_count += count;
                            metrics::PROCESSED_TXNS_AUTHENTICATOR
                                .with_label_values(&[process_type, "Ed25519_in_MultiEd25519"])
                                .inc_by(count as u64);
                        },
                        AccountAuthenticator::SingleKey { authenticator } => {
                            signature_count += 1;
                            metrics::PROCESSED_TXNS_AUTHENTICATOR
                                .with_label_values(&[
                                    process_type,
                                    &format!("{}_in_SingleKey", authenticator.signature().name()),
                                ])
                                .inc();
                        },
                        AccountAuthenticator::MultiKey { authenticator } => {
                            for (_, signature) in authenticator.signatures() {
                                signature_count += 1;
                                metrics::PROCESSED_TXNS_AUTHENTICATOR
                                    .with_label_values(&[
                                        process_type,
                                        &format!("{}_in_MultiKey", signature.name()),
                                    ])
                                    .inc();
                            }
                        },
                        AccountAuthenticator::NoAccountAuthenticator => {
                            metrics::PROCESSED_TXNS_AUTHENTICATOR
                                .with_label_values(&[process_type, "NoAccountAuthenticator"])
                                .inc();
                        },
                    };
                }

                metrics::PROCESSED_TXNS_NUM_AUTHENTICATORS
                    .with_label_values(&[process_type])
                    .observe(signature_count as f64);
            }

            match user_txn.payload() {
                aptos_types::transaction::TransactionPayload::Script(_script) => {
                    metrics::PROCESSED_USER_TXNS_BY_PAYLOAD
                        .with_label_values(&[process_type, "script", state])
                        .inc();
                },
                aptos_types::transaction::TransactionPayload::EntryFunction(function) => {
                    metrics::PROCESSED_USER_TXNS_BY_PAYLOAD
                        .with_label_values(&[process_type, "function", state])
                        .inc();

                    let is_core = function.module().address() == &CORE_CODE_ADDRESS;
                    metrics::PROCESSED_USER_TXNS_ENTRY_FUNCTION_BY_MODULE
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
                        metrics::PROCESSED_USER_TXNS_ENTRY_FUNCTION_BY_CORE_METHOD
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
                    metrics::PROCESSED_USER_TXNS_BY_PAYLOAD
                        .with_label_values(&[process_type, "multisig", state])
                        .inc();
                },

                // Deprecated.
                aptos_types::transaction::TransactionPayload::ModuleBundle(_) => {
                    metrics::PROCESSED_USER_TXNS_BY_PAYLOAD
                        .with_label_values(&[process_type, "deprecated_module_bundle", state])
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
            metrics::PROCESSED_USER_TXNS_CORE_EVENTS
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
