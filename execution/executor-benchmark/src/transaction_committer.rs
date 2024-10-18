// Copyright © Aptos Foundation
// Parts of the project are originally copyright © Meta Platforms, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{metrics::NUM_TXNS, pipeline::CommitBlockMessage};
use aptos_crypto::hash::HashValue;
use aptos_db::metrics::API_LATENCY_SECONDS;
use aptos_executor::{
    block_executor::{BlockExecutor, TransactionBlockExecutor},
    metrics::{COMMIT_BLOCKS, EXECUTE_BLOCK, VM_EXECUTE_BLOCK},
};
use aptos_executor_types::BlockExecutorTrait;
use aptos_logger::prelude::*;
use aptos_types::{
    aggregate_signature::AggregateSignature,
    block_info::BlockInfo,
    ledger_info::{LedgerInfo, LedgerInfoWithSignatures},
    transaction::Version,
};
use std::{
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

pub(crate) fn gen_li_with_sigs(
    block_id: HashValue,
    root_hash: HashValue,
    version: Version,
) -> LedgerInfoWithSignatures {
    let block_info = BlockInfo::new(
        1,        /* epoch */
        0,        /* round, doesn't matter */
        block_id, /* id, doesn't matter */
        root_hash, version, 0,    /* timestamp_usecs, doesn't matter */
        None, /* next_epoch_state */
    );
    let ledger_info = LedgerInfo::new(
        block_info,
        HashValue::zero(), /* consensus_data_hash, doesn't matter */
    );
    LedgerInfoWithSignatures::new(
        ledger_info,
        AggregateSignature::empty(), /* signatures */
    )
}

pub struct TransactionCommitter<V> {
    executor: Arc<BlockExecutor<V>>,
    version: Version,
    block_receiver: mpsc::Receiver<CommitBlockMessage>,
}

impl<V> TransactionCommitter<V>
where
    V: TransactionBlockExecutor,
{
    pub fn new(
        executor: Arc<BlockExecutor<V>>,
        version: Version,
        block_receiver: mpsc::Receiver<CommitBlockMessage>,
    ) -> Self {
        Self {
            version,
            executor,
            block_receiver,
        }
    }

    pub fn run(&mut self) {
        let start_version = self.version;
        info!("Start with version: {}", start_version);

        while let Ok(msg) = self.block_receiver.recv() {
            info!("Received commit block message: {:?}. num_txns: {:?}.", msg.block_id, msg.num_txns);
            let CommitBlockMessage {
                block_id,
                root_hash,
                first_block_start_time,
                current_block_start_time,
                partition_time,
                execution_time,
                num_txns,
            } = msg;
            NUM_TXNS
                .with_label_values(&["commit"])
                .inc_by(num_txns as u64);

            self.version += num_txns as u64;
            let commit_start = std::time::Instant::now();
            let ledger_info_with_sigs = gen_li_with_sigs(block_id, root_hash, self.version);
            let parent_block_id = self.executor.committed_block_id();
            self.executor
                .pre_commit_block(block_id, parent_block_id)
                .unwrap();
            info!("pre_commit_block");
            self.executor.commit_ledger(ledger_info_with_sigs).unwrap();
            info!("commit_ledger");
            report_block(
                start_version,
                self.version,
                first_block_start_time,
                current_block_start_time,
                partition_time,
                execution_time,
                Instant::now().duration_since(commit_start),
                num_txns,
            );
            info!("report_block");
        }
    }
}

fn report_block(
    start_version: Version,
    version: Version,
    first_block_start_time: Instant,
    current_block_start_time: Instant,
    partition_time: Duration,
    execution_time: Duration,
    commit_time: Duration,
    block_size: usize,
) {
    let total_versions = (version - start_version) as f64;
    info!(
        "Version: {}. latency: {} ms, partition time: {} ms, execute time: {} ms. commit time: {} ms. TPS: {:.0} (partition: {:.0}, execution: {:.0}, commit: {:.0}). Accumulative TPS: {:.0}",
        version,
        Instant::now().duration_since(current_block_start_time).as_millis(),
        partition_time.as_millis(),
        execution_time.as_millis(),
        commit_time.as_millis(),
        block_size as f64 / (std::cmp::max(std::cmp::max(partition_time, execution_time), commit_time)).as_secs_f64(),
        block_size as f64 / partition_time.as_secs_f64(),
        block_size as f64 / execution_time.as_secs_f64(),
        block_size as f64 / commit_time.as_secs_f64(),
        total_versions / first_block_start_time.elapsed().as_secs_f64(),
    );
    info!(
            "Accumulative total: VM time: {:.0} secs, executor time: {:.0} secs, commit time: {:.0} secs, DB commit time: {:.0} secs",
            VM_EXECUTE_BLOCK.get_sample_sum(),
            EXECUTE_BLOCK.get_sample_sum() - VM_EXECUTE_BLOCK.get_sample_sum(),
            COMMIT_BLOCKS.get_sample_sum(),
            API_LATENCY_SECONDS.get_metric_with_label_values(&["save_transactions", "Ok"]).expect("must exist.").get_sample_sum(),
        );
    const NANOS_PER_SEC: f64 = 1_000_000_000.0;
    info!(
            "Accumulative per transaction: VM time: {:.0} ns, executor time: {:.0} ns, commit time: {:.0} ns, DB commit time: {:.0} ns",
            VM_EXECUTE_BLOCK.get_sample_sum() * NANOS_PER_SEC
                / total_versions,
            (EXECUTE_BLOCK.get_sample_sum() - VM_EXECUTE_BLOCK.get_sample_sum()) * NANOS_PER_SEC
                / total_versions,
            COMMIT_BLOCKS.get_sample_sum() * NANOS_PER_SEC
                / total_versions,
            API_LATENCY_SECONDS.get_metric_with_label_values(&["save_transactions", "Ok"]).expect("must exist.").get_sample_sum() * NANOS_PER_SEC
                / total_versions,
        );
}
