// Copyright © Aptos Foundation
// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0
use crate::{remote_state_view_service::RemoteStateViewService, ExecuteBlockCommand, RemoteExecutionRequest, RemoteExecutionResult, RemoteExecutionRequestRef, ExecuteBlockCommandRef};
use aptos_logger::{info, trace};
use aptos_secure_net::network_controller::{Message, MessageType, NetworkController, OutboundRpcScheduler};
use aptos_storage_interface::cached_state_view::CachedStateView;
use aptos_types::{
    block_executor::{
        config::BlockExecutorConfigFromOnchain, partitioner::{PartitionedTransactions, PartitionedTransactionsV3},
    },
    state_store::StateView,
    transaction::TransactionOutput,
    vm_status::VMStatus,
    write_set::TOTAL_SUPPLY_STATE_KEY,
};
use aptos_vm::sharded_block_executor::{
    executor_client::{ExecutorClient, ShardedExecutionOutput},
    ShardedBlockExecutor,
};
use crossbeam_channel::{Receiver, Sender};
use once_cell::sync::{Lazy, OnceCell};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    thread,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::SystemTime;
use itertools::Itertools;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng, thread_rng};
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator};
use rayon::slice::ParallelSlice;
use aptos_drop_helper::DEFAULT_DROPPER;
use aptos_secure_net::grpc_network_service::outbound_rpc_helper::OutboundRpcHelper;
use aptos_secure_net::network_controller::metrics::{get_delta_time, REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER};
use aptos_types::block_executor::partitioner::PartitionV3;
use aptos_types::transaction::analyzed_transaction::AnalyzedTransaction;
use aptos_vm::sharded_block_executor::aggr_overridden_state_view::TOTAL_SUPPLY_AGGR_BASE_VAL;
use aptos_vm::sharded_block_executor::sharded_aggregator_service::{DeltaU128, get_state_value};
use aptos_vm::sharded_block_executor::sharded_executor_service::{CmdsAndMetaDataRef, TransactionIdxAndOutput, V3CmdsOrMetaDataRef, V3CmdsRef, V3MetaDataRef};
use crate::metrics::REMOTE_EXECUTOR_TIMER;

pub static COORDINATOR_PORT: u16 = 52200;

static REMOTE_ADDRESSES: OnceCell<Vec<SocketAddr>> = OnceCell::new();
static COORDINATOR_ADDRESS: OnceCell<SocketAddr> = OnceCell::new();

pub fn set_remote_addresses(addresses: Vec<SocketAddr>) {
    REMOTE_ADDRESSES.set(addresses).ok();
}

pub fn get_remote_addresses() -> Vec<SocketAddr> {
    match REMOTE_ADDRESSES.get() {
        Some(value) => value.clone(),
        None => vec![],
    }
}

pub fn set_coordinator_address(address: SocketAddr) {
    COORDINATOR_ADDRESS.set(address).ok();
}

pub fn get_coordinator_address() -> SocketAddr {
    match COORDINATOR_ADDRESS.get() {
        Some(value) => *value,
        None => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), COORDINATOR_PORT),
    }
}

pub static REMOTE_SHARDED_BLOCK_EXECUTOR: Lazy<
    Arc<
        aptos_infallible::Mutex<
            ShardedBlockExecutor<CachedStateView, RemoteExecutorClient<CachedStateView>>,
        >,
    >,
> = Lazy::new(|| {
    info!("REMOTE_SHARDED_BLOCK_EXECUTOR created");
    Arc::new(aptos_infallible::Mutex::new(
        RemoteExecutorClient::create_remote_sharded_block_executor(
            get_coordinator_address(),
            get_remote_addresses(),
            None,
        ),
    ))
});

#[allow(dead_code)]
pub struct RemoteExecutorClient<S: StateView + Sync + Send + 'static> {
    // The network controller used to create channels to send and receive messages. We want the
    // network controller to be owned by the executor client so that it is alive for the entire
    // lifetime of the executor client.
    network_controller: NetworkController,
    state_view_service: Arc<RemoteStateViewService<S>>,
    // Channels to send execute block commands to the executor shards.
    command_txs: Arc<Vec<Vec<Arc<tokio::sync::Mutex<OutboundRpcHelper>>>>>,
    // Channels to receive execution results from the executor shards.
    result_rxs: Vec<Receiver<Message>>,
    // Thread pool used to pre-fetch the state values for the block in parallel and create an in-memory state view.
    thread_pool: Arc<rayon::ThreadPool>,
    cmd_tx_thread_pool: Arc<rayon::ThreadPool>,

    phantom: std::marker::PhantomData<S>,
    _join_handle: Option<thread::JoinHandle<()>>,
    outbound_rpc_scheduler: Arc<OutboundRpcScheduler>
}

#[allow(dead_code)]
impl<S: StateView + Sync + Send + 'static> RemoteExecutorClient<S> {
    pub fn new(
        remote_shard_addresses: Vec<SocketAddr>,
        mut controller: NetworkController,
        num_threads: Option<usize>,
    ) -> Self {
        let num_threads = num_threads.unwrap_or_else(num_cpus::get);
        let thread_pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(24) //num_threads)
                .build()
                .unwrap(),
        );
        let outbound_rpc_runtime = controller.get_outbound_rpc_runtime();
        let self_addr = controller.get_self_addr();
        let controller_mut_ref = &mut controller;
        let num_shards = remote_shard_addresses.len();
        let (command_txs, result_rxs) = remote_shard_addresses
            .iter()
            .enumerate()
            .map(|(shard_id, address)| {
                let execute_command_type = format!("execute_command_{}", shard_id);
                let execute_result_type = format!("execute_result_{}", shard_id);
                let mut command_tx = vec![];
                for _ in 0..std::cmp::max(1, num_threads/(2 * num_shards)) {
                    command_tx.push(Arc::new(tokio::sync::Mutex::new(OutboundRpcHelper::new(self_addr, *address, outbound_rpc_runtime.clone()))));
                }
                let result_rx = controller_mut_ref.create_inbound_channel(execute_result_type);
                (command_tx, result_rx)
            })
            .unzip();

        let state_view_service = Arc::new(RemoteStateViewService::new(
            controller_mut_ref,
            remote_shard_addresses,
            None,
        ));

        let state_view_service_clone = state_view_service.clone();

        let join_handle = thread::Builder::new()
            .name("remote-state_view-service".to_string())
            .spawn(move || state_view_service_clone.start())
            .unwrap();

        controller.start();

        let cmd_tx_thread_pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .thread_name(move |index| format!("rmt-exe-cli-cmd-tx-{}", index))
                .num_threads(num_shards) //num_cpus::get() / 2)
                .build()
                .unwrap(),
        );
        let scheduler = controller.get_outbound_rpc_scheduler();
        Self {
            network_controller: controller,
            state_view_service,
            _join_handle: Some(join_handle),
            command_txs: Arc::new(command_txs),
            result_rxs,
            thread_pool,
            cmd_tx_thread_pool,
            phantom: std::marker::PhantomData,
            outbound_rpc_scheduler: scheduler,
        }
    }

    pub fn create_remote_sharded_block_executor(
        coordinator_address: SocketAddr,
        remote_shard_addresses: Vec<SocketAddr>,
        num_threads: Option<usize>,
    ) -> ShardedBlockExecutor<S, RemoteExecutorClient<S>> {
        ShardedBlockExecutor::new(RemoteExecutorClient::new(
            remote_shard_addresses,
            NetworkController::new(
                "remote-executor-coordinator".to_string(),
                coordinator_address,
                5000,
            ),
            num_threads,
        ))
    }

    fn get_output_from_shards(&self) -> Result<Vec<Vec<Vec<TransactionOutput>>>, VMStatus> {
        trace!("RemoteExecutorClient Waiting for results");
        /*let thread_pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(self.num_shards())
                .build()
                .unwrap(),
        );

        let mut results = vec![];
        for rx in self.result_rxs.iter() {
            let received_bytes = rx.recv().unwrap().to_bytes();
            let result: RemoteExecutionResult = bcs::from_bytes(&received_bytes).unwrap();
            results.push(result.inner?);
        }*/

        let results: Vec<(usize, Vec<Vec<TransactionOutput>>)> = (0..self.num_shards()).into_par_iter().map(|shard_id| {
            let received_msg = self.result_rxs[shard_id].recv().unwrap();
            let delta = get_delta_time(received_msg.start_ms_since_epoch.unwrap());
            REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
                .with_label_values(&["9_1_results_tx_msg_remote_exe_recv"]).observe(delta as f64);

            let bcs_deser_timer = REMOTE_EXECUTOR_TIMER
                .with_label_values(&["0", "result_rx_bcs_deser"])
                .start_timer();
            let result: RemoteExecutionResult = bcs::from_bytes(&received_msg.to_bytes()).unwrap();
            drop(bcs_deser_timer);
            (shard_id, result.inner.unwrap())
        }).collect();

        let _timer = REMOTE_EXECUTOR_TIMER
            .with_label_values(&["0", "result_rx_gather"])
            .start_timer();
        let mut res: Vec<Vec<Vec<TransactionOutput>>> = vec![vec![]; self.num_shards()];
        for (shard_id, result) in results.into_iter() {
            res[shard_id] = result;
        }
        Ok(res)
    }

    fn get_streamed_output_from_shards(&self, expected_outputs: Vec<u64>, duration_since_epoch: u64,
                                       total_supply_base_val: u128) -> Result<Vec<TransactionOutput>, VMStatus> {
        #[derive(Copy, Clone)]
        struct Pointer(*mut TransactionOutput);
        unsafe impl Send for Pointer {}
        unsafe impl Sync for Pointer {}

        let total_expected_outputs = expected_outputs.iter().sum::<u64>();
        let results_recvd: Vec<Arc<AtomicBool>> = (0..total_expected_outputs)
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect();
        let shared_results_recvd = Arc::new(results_recvd);
        let shared_results_recvd_clone = shared_results_recvd.clone();

        let mut results = vec![TransactionOutput::default(); total_expected_outputs as usize];
        let results_ptr = Pointer(results.as_mut_ptr());
        let num_deser_threads = self.num_shards();

        let (aggr_start_tx, aggr_start_rx) = mpsc::channel();
        let (aggr_tx, aggr_rx) = mpsc::channel();
        let (deser_tx, deser_rx) = crossbeam_channel::unbounded();
        let (deser_finished_tx, deser_finished_rx) = crossbeam_channel::unbounded();
        let deser_finished_tx = Arc::new(deser_finished_tx);

        self.cmd_tx_thread_pool.spawn(move || {
            aggr_start_rx.recv().unwrap();
            // loop to see if results are received; as results are received update the total supply
            let mut curr_txn_idx = 0;
            let mut aggr_total_supply_delta = total_supply_base_val;
            loop {
                if shared_results_recvd_clone[curr_txn_idx].load(std::sync::atomic::Ordering::Relaxed) == false {
                    // probably busy wait is good for perf here
                    continue;
                }
                let mut result = &mut results[curr_txn_idx];
                if let Some(total_supply) = result.total_supply_delta {
                    aggr_total_supply_delta = total_supply.add(aggr_total_supply_delta);
                    result.update_total_supply(aggr_total_supply_delta);
                }
                curr_txn_idx += 1;
                if curr_txn_idx == total_expected_outputs as usize {
                    break;
                }
            }
            aggr_tx.send(results).unwrap();
        });

        for _ in 0..num_deser_threads {
            let results_clone = results_ptr.clone();
            let deser_rx_clone: Receiver<Message> = deser_rx.clone();
            let deser_finished_tx_clone = deser_finished_tx.clone();
            let shared_results_recvd_clone = shared_results_recvd.clone();
            self.cmd_tx_thread_pool.spawn(move || {
                while let Ok(msg) = deser_rx_clone.recv() {
                    let bcs_deser_timer = REMOTE_EXECUTOR_TIMER
                        .with_label_values(&["0", "result_rx_bcs_deser"])
                        .start_timer();
                    let result: Vec<TransactionIdxAndOutput> = bcs::from_bytes(&msg.to_bytes()).unwrap();
                    drop(bcs_deser_timer);

                    for txn_output in result {
                        let txn_idx = txn_output.txn_idx as usize;
                        // correctness guaranteed by disjointness of txn indices
                        unsafe { *{results_clone}.0.wrapping_add(txn_idx) = txn_output.txn_output; }
                        shared_results_recvd_clone[txn_idx].store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                deser_finished_tx_clone.send(()).unwrap();
            });
        }
        let mut result_recv_start: AtomicBool = AtomicBool::new(false);
        (0..self.num_shards()).into_par_iter().for_each(|shard_id| {
            let mut num_outputs_received: u64 = 0;
            loop {
                let received_msg = self.result_rxs[shard_id].recv().unwrap();
                let num_txn = received_msg.seq_num.unwrap(); // seq_num field is used to pass num_txn in the network message
                num_outputs_received += num_txn;
                deser_tx.send(received_msg).unwrap();
                if !result_recv_start.load(std::sync::atomic::Ordering::Relaxed) {
                    result_recv_start.store(true, std::sync::atomic::Ordering::Relaxed);
                    aggr_start_tx.send(()).unwrap();
                }
                if num_outputs_received == expected_outputs[shard_id] {
                    let delta = get_delta_time(duration_since_epoch);
                    REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
                        .with_label_values(&["9_1_results_tx_msg_remote_exe_recv"]).observe(delta as f64);
                    break;
                }
            }
        });
        drop(deser_tx);
        let mut cnt = 0;
        while let Ok(msg) = deser_finished_rx.recv() {
            cnt += 1;
            if cnt == num_deser_threads {
                break;
            }
        }
        let delta = get_delta_time(duration_since_epoch);
        REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
            .with_label_values(&["9_2_results_rx_all_shards"]).observe(delta as f64);

        let results = aggr_rx.recv().unwrap();
        let delta = get_delta_time(duration_since_epoch);
        REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
            .with_label_values(&["9_3_aggr_total_supply_done"]).observe(delta as f64);
        Ok(results)
    }
}

impl<S: StateView + Sync + Send + 'static> ExecutorClient<S> for RemoteExecutorClient<S> {
    fn is_remote_executor_client(&self) -> bool {
        true
    }

    fn num_shards(&self) -> usize {
        self.command_txs.len()
    }

    fn execute_block(&self, state_view: Arc<S>, transactions: PartitionedTransactions, concurrency_level_per_shard: usize, onchain_config: BlockExecutorConfigFromOnchain) -> Result<ShardedExecutionOutput, VMStatus> {
        panic!("Not implemented for RemoteExecutorClient");
    }

    fn execute_block_remote(
        &self,
        state_view: Arc<S>,
        transactions: Arc<PartitionedTransactions>,
        concurrency_level_per_shard: usize,
        onchain_config: BlockExecutorConfigFromOnchain,
        duration_since_epoch: u64
    ) -> Result<Vec<TransactionOutput>, VMStatus> {
        trace!("RemoteExecutorClient Sending block to shards");
        let total_supply_base_val: u128 = get_state_value(&TOTAL_SUPPLY_STATE_KEY, state_view.as_ref()).unwrap();
        let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as u64;
        trace!("Executing block started at time {}", current_time);
        self.state_view_service.set_state_view(state_view);
        let partitions = &transactions.as_v3_ref().partitions;

        let cmd_tx_timer = REMOTE_EXECUTOR_TIMER
            .with_label_values(&["0", "cmd_tx_async"])
            .start_timer();

        REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
            .with_label_values(&["0_cmd_tx_start"]).observe(get_delta_time(duration_since_epoch) as f64);

        let mut expected_outputs = vec![0; self.num_shards()];
        let batch_size = 200;
        for (shard_id, _) in partitions.into_iter().enumerate() {
            let num_txns = partitions[shard_id].txns.len();
            expected_outputs[shard_id] = num_txns as u64;
            // TODO: Check if the function can get Arc<BlockExecutorConfigFromOnchain> instead.
            let onchain_config_clone = onchain_config.clone();
            let transactions_clone = transactions.clone();

            let senders = self.command_txs.clone();

            let outbound_rpc_scheduler_clone = self.outbound_rpc_scheduler.clone();
            self.cmd_tx_thread_pool.spawn(move || {
                {
                    // send the metadata to the remote executor
                    let PartitionV3 {
                        block_id: _,
                        txns: shard_txns,
                        global_idxs,
                        local_idx_by_global,
                        key_sets_by_dep,
                        follower_shard_sets,
                    } = &transactions_clone.as_v3_ref().partitions[shard_id];
                    let execution_metadata = V3CmdsOrMetaDataRef::MetaData(
                        V3MetaDataRef {
                            num_txns: shard_txns.len(),
                            global_idxs,
                            local_idx_by_global,
                            key_sets_by_dep,
                            follower_shard_sets,
                            onchain_config: &onchain_config_clone,
                        });
                    let msg = Message::create_with_metadata(bcs::to_bytes(&execution_metadata).unwrap(), duration_since_epoch, 0, 0);
                    //info!("********* Metadata sent to shard {}; metadata size {}", shard_id, msg.data.len());

                    REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
                        .with_label_values(&["1a_cmd_v3metadata_msg_send"]).observe(get_delta_time(duration_since_epoch) as f64);
                    let execute_command_type = format!("execute_command_{}", shard_id);
                    /*senders[shard_id][0]
                        .lock()
                        .unwrap()
                        .send(msg, &MessageType::new(execute_command_type));*/
                    outbound_rpc_scheduler_clone.send(msg,
                                                     MessageType::new(execute_command_type),
                                                     senders[shard_id][0].clone(),
                                                     0);
                }

                //let shard_txns = &transactions_clone.get_ref().0[shard_id].sub_blocks[0].transactions;
                //let index_offset = transactions_clone.get_ref().0[shard_id].sub_blocks[0].start_index as usize;
                let txns_clone = transactions_clone.clone();
                let shard_txns = &txns_clone.as_v3_ref().partitions[shard_id].txns;

                let _ = shard_txns
                    .chunks(batch_size)
                    .enumerate()
                    .for_each(|(chunk_idx, txns)| {
                        let analyzed_txns = txns.iter().map(|txn| {
                            txn
                        }).collect::<Vec<&AnalyzedTransaction>>();
                        let execution_batch_req = V3CmdsOrMetaDataRef::Cmds(
                            V3CmdsRef {
                                cmds: &analyzed_txns,
                                num_txns_total: num_txns,
                                batch_start_index: chunk_idx * batch_size,
                        });
                        let bcs_ser_timer = REMOTE_EXECUTOR_TIMER
                            .with_label_values(&["0", "cmd_tx_bcs_ser"])
                            .start_timer();
                        let msg = Message::create_with_metadata(bcs::to_bytes(&execution_batch_req).unwrap(), duration_since_epoch, analyzed_txns.len() as u64, (chunk_idx + 1) as u64);
                        //info!("********* Cmds sent to shard {}; size {}", shard_id, msg.data.len());
                        drop(bcs_ser_timer);
                        REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
                            .with_label_values(&["1_cmd_tx_msg_send"]).observe(get_delta_time(duration_since_epoch) as f64);
                        let execute_command_type = format!("execute_command_{}", shard_id);
                        let rand_send_thread_idx = thread_rng().gen_range(0, senders[shard_id].len());

                        let timer_1 = REMOTE_EXECUTOR_TIMER
                            .with_label_values(&["0", "cmd_tx_lock_send"])
                            .start_timer();
                        // senders[shard_id][rand_send_thread_idx]
                        //     .lock()
                        //     .unwrap()
                        //     .send(msg, &MessageType::new(execute_command_type));
                        outbound_rpc_scheduler_clone.send(msg,
                                                         MessageType::new(execute_command_type),
                                                         senders[shard_id][rand_send_thread_idx].clone(),
                                                         chunk_idx as u64);
                        drop(timer_1)
                    });
            });
        }

        drop(cmd_tx_timer);

        //let execution_results = self.get_output_from_shards()?;
        info!("Waiting to receive results from shards");
        let results = self.get_streamed_output_from_shards(
            expected_outputs, duration_since_epoch, total_supply_base_val);

        let timer = REMOTE_EXECUTOR_TIMER
            .with_label_values(&["0", "drop_state_view_finally"])
            .start_timer();
        self.state_view_service.drop_state_view();
        drop(timer);
        REMOTE_EXECUTOR_CMD_RESULTS_RND_TRP_JRNY_TIMER
            .with_label_values(&["9_8_execute_remote_block_done"]).observe(get_delta_time(duration_since_epoch) as f64);
        DEFAULT_DROPPER.schedule_drop(transactions);
        results
        //Ok(ShardedExecutionOutput::new(execution_results, vec![]))
    }

    fn shutdown(&mut self) {
        self.network_controller.shutdown();
    }

    fn execute_block_v3(
        &self,
        _state_view: Arc<S>,
        _transactions: PartitionedTransactionsV3,
        _concurrency_level_per_shard: usize,
        _onchain_config: BlockExecutorConfigFromOnchain,
    ) -> Result<Vec<TransactionOutput>, VMStatus> {
        todo!()
    }
}
