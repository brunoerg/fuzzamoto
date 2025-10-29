#[cfg(any(feature = "oracle_netsplit", feature = "oracle_consensus"))]
use std::time::{Duration, Instant};

#[cfg(feature = "nyx")]
use fuzzamoto_nyx_sys::*;

use bitcoin::hashes::Hash;
use fuzzamoto::{
    connections::Transport,
    fuzzamoto_main,
    oracles::{CrashOracle, Oracle, OracleResult},
    scenarios::{Scenario, ScenarioInput, ScenarioResult, generic::GenericScenario},
    targets::{BitcoinCoreTarget, CallRpc, ConnectableTarget, HasTipHash, Target},
};

#[cfg(feature = "oracle_netsplit")]
use fuzzamoto::oracles::{NetSplitContext, NetSplitOracle};

#[cfg(feature = "oracle_consensus")]
use fuzzamoto::oracles::{ConsensusContext, ConsensusOracle};

use fuzzamoto_ir::{
    Program, ProgramContext,
    compiler::{CompiledAction, CompiledProgram, Compiler},
};

const COINBASE_MATURITY_HEIGHT_LIMIT: u32 = 100;
const LATE_BLOCK_HEIGHT_LIMIT: u32 = 190;
const COINBASE_VALUE: u64 = 25 * 100_000_000;
const OP_TRUE_SCRIPT_PUBKEY: [u8; 34] = [
    0u8, 32, 74, 232, 21, 114, 240, 110, 27, 136, 253, 92, 237, 122, 26, 0, 9, 69, 67, 46, 131,
    225, 85, 30, 111, 114, 30, 233, 192, 11, 140, 195, 50, 96,
];

/// Hybrid IR scenario that combines P2P messages with RPC calls
struct IrScenario<TX: Transport, T: Target<TX> + ConnectableTarget> {
    inner: GenericScenario<TX, T>,
    #[cfg(any(feature = "oracle_netsplit", feature = "oracle_consensus"))]
    second: T,
}

/// Extended compiled action that includes RPC calls
#[derive(Debug)]
enum HybridAction {
    P2P(CompiledAction),
    RpcGetMempoolInfo,
}

pub struct TestCase {
    program: CompiledProgram,
    // Add RPC call points - indices in the action sequence where RPC should be called
    rpc_call_points: Vec<usize>,
}

impl<'a> ScenarioInput<'a> for TestCase {
    fn decode(bytes: &'a [u8]) -> Result<Self, String> {
        // First byte(s) determine number of RPC calls
        if bytes.is_empty() {
            return Err("Empty input".to_string());
        }

        let num_rpc_calls = (bytes[0] % 10) as usize; // Max 10 RPC calls
        let mut rpc_call_points = Vec::new();

        /*
        // Workaround to have less RPC calls
        if num_rpc_calls > 0 {
            num_rpc_calls = 1;
        }*/

        // Next bytes determine where to insert RPC calls
        for i in 0..num_rpc_calls {
            if i + 1 < bytes.len() {
                rpc_call_points.push(bytes[i + 1] as usize);
            }
        }

        // Rest is the IR program
        let program_bytes = &bytes[(num_rpc_calls + 1).min(bytes.len())..];

        let program = if cfg!(feature = "compile_in_vm") {
            let program: Program =
                postcard::from_bytes(program_bytes).map_err(|e| e.to_string())?;
            let mut compiler = Compiler::new();
            compiler.compile(&program).map_err(|e| e.to_string())?
        } else {
            postcard::from_bytes(program_bytes).map_err(|e| e.to_string())?
        };

        Ok(Self {
            program,
            rpc_call_points,
        })
    }
}

impl<TX, T> IrScenario<TX, T>
where
    TX: Transport,
    T: Target<TX> + HasTipHash + ConnectableTarget + CallRpc,
{
    fn build_program_context(inner: &GenericScenario<TX, T>) -> ProgramContext {
        ProgramContext {
            num_nodes: 1,
            num_connections: inner.connections.len(),
            timestamp: inner.time,
        }
    }

    fn build_txos(inner: &GenericScenario<TX, T>) -> Vec<fuzzamoto_ir::Txo> {
        let mut txos = Vec::new();
        for (block, _height) in inner
            .block_tree
            .values()
            .filter(|(_, height)| *height < COINBASE_MATURITY_HEIGHT_LIMIT)
        {
            let coinbase = block.coinbase().unwrap();
            let mut hash = [0u8; 32];
            hash.copy_from_slice(
                coinbase
                    .compute_txid()
                    .as_raw_hash()
                    .as_byte_array()
                    .as_slice(),
            );

            txos.push(fuzzamoto_ir::Txo {
                outpoint: (hash, 0u32),
                value: COINBASE_VALUE,
                script_pubkey: OP_TRUE_SCRIPT_PUBKEY.to_vec(),
                spending_script_sig: vec![],
                spending_witness: vec![vec![0x51]],
            });
        }
        txos
    }

    fn build_headers(inner: &GenericScenario<TX, T>) -> Vec<fuzzamoto_ir::Header> {
        inner
            .block_tree
            .values()
            .filter(|(_, height)| *height > LATE_BLOCK_HEIGHT_LIMIT)
            .map(|(block, height)| fuzzamoto_ir::Header {
                prev: *block.header.prev_blockhash.as_byte_array(),
                merkle_root: *block.header.merkle_root.as_byte_array(),
                nonce: block.header.nonce,
                bits: block.header.bits.to_consensus(),
                time: block.header.time,
                version: block.header.version.to_consensus(),
                height: *height,
            })
            .collect()
    }

    fn dump_context(
        context: ProgramContext,
        txos: Vec<fuzzamoto_ir::Txo>,
        headers: Vec<fuzzamoto_ir::Header>,
    ) -> Result<(), String> {
        let full_context = postcard::to_allocvec(&fuzzamoto_ir::FullProgramContext {
            context,
            txos,
            headers,
        })
        .map_err(|e| e.to_string())?;

        #[cfg(feature = "nyx")]
        {
            const CONTEXT_FILE_NAME: &str = "ir.context";
            unsafe {
                nyx_dump_file_to_host(
                    CONTEXT_FILE_NAME.as_ptr() as *const i8,
                    CONTEXT_FILE_NAME.len(),
                    full_context.as_ptr(),
                    full_context.len(),
                );
            }
        }

        #[cfg(not(feature = "nyx"))]
        if let Ok(context_file) = std::env::var("DUMP_CONTEXT") {
            std::fs::write(context_file, &full_context).map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    #[cfg(any(feature = "oracle_netsplit", feature = "oracle_consensus"))]
    fn create_and_sync_second_target(args: &[String], primary: &T) -> Result<T, String> {
        let mut second = if args.len() > 2 {
            T::from_path(&args[2])?
        } else {
            T::from_path(&args[1])?
        };
        second.connect_to(primary)?;
        Self::sync_nodes(primary, &mut second)?;
        Ok(second)
    }

    #[cfg(any(feature = "oracle_netsplit", feature = "oracle_consensus"))]
    fn sync_nodes(primary: &T, reference: &mut T) -> Result<(), String> {
        const SYNC_TIMEOUT: Duration = Duration::from_secs(10);
        const POLL_INTERVAL: Duration = Duration::from_millis(10);

        let start = Instant::now();
        let mut synced = false;

        while start.elapsed() < SYNC_TIMEOUT {
            let primary_tip = primary.get_tip_hash();
            let reference_tip = reference.get_tip_hash();

            if primary_tip.is_some() && primary_tip == reference_tip {
                log::info!("Nodes synced successfully!");
                synced = true;
                break;
            }

            std::thread::sleep(POLL_INTERVAL);
        }

        if !synced {
            return Err("nodes failed to sync".to_string());
        }

        Ok(())
    }

    /// Build the hybrid action sequence by interleaving P2P and RPC actions
    fn build_hybrid_actions(&self, testcase: &TestCase) -> Vec<HybridAction> {
        let mut hybrid_actions = Vec::new();
        let total_actions = testcase.program.actions.len();

        // Convert P2P actions
        for (idx, action) in testcase.program.actions.iter().enumerate() {
            // Check if we should insert an RPC call at this position
            if testcase.rpc_call_points.contains(&idx) {
                hybrid_actions.push(HybridAction::RpcGetMempoolInfo);
            }
            hybrid_actions.push(HybridAction::P2P(action.clone()));
        }

        // Add any remaining RPC calls at the end
        for &call_point in &testcase.rpc_call_points {
            if call_point >= total_actions {
                hybrid_actions.push(HybridAction::RpcGetMempoolInfo);
            }
        }

        hybrid_actions
    }

    fn process_hybrid_actions(&mut self, actions: Vec<HybridAction>) {
        for action in actions {
            match action {
                HybridAction::P2P(CompiledAction::SendRawMessage(from, command, message)) => {
                    if self.inner.connections.is_empty() {
                        continue;
                    }

                    let num_connections = self.inner.connections.len();
                    if let Some(connection) = self.inner.connections.get_mut(from % num_connections)
                    {
                        if cfg!(feature = "force_send_and_ping") {
                            let _ = connection.send_and_ping(&(command, message));
                        } else {
                            let _ = connection.send(&(command, message));
                        }
                    }
                }
                HybridAction::P2P(CompiledAction::SetTime(time)) => {
                    let _ = self.inner.target.set_mocktime(time);
                    #[cfg(any(feature = "oracle_netsplit", feature = "oracle_consensus"))]
                    let _ = self.second.set_mocktime(time);
                }
                HybridAction::RpcGetMempoolInfo => {
                    let _ = self.inner.target.call_rpc("getmempoolinfo", &[]);
                }
                _ => {}
            }
        }
    }

    fn ping_connections(&mut self) {
        for connection in self.inner.connections.iter_mut() {
            let _ = connection.ping();
        }
    }

    fn evaluate_oracles(&mut self) -> ScenarioResult {
        let crash_oracle = CrashOracle::<TX>::default();
        if let OracleResult::Fail(e) = crash_oracle.evaluate(&self.inner.target) {
            return ScenarioResult::Fail(format!("{}", e));
        }

        #[cfg(feature = "oracle_netsplit")]
        {
            let net_split_oracle = NetSplitOracle::<TX, TX>::default();
            if let OracleResult::Fail(e) = net_split_oracle.evaluate(&NetSplitContext {
                primary: &self.inner.target,
                reference: &self.second,
            }) {
                return ScenarioResult::Fail(format!("{}", e));
            }
        }

        #[cfg(feature = "oracle_consensus")]
        {
            if !self.second.is_connected_to(&self.inner.target) {
                let _ = self.second.connect_to(&self.inner.target);
            }

            let consensus_oracle = ConsensusOracle::<TX, TX>::default();
            if let OracleResult::Fail(e) = consensus_oracle.evaluate(&ConsensusContext {
                primary: &self.inner.target,
                reference: &self.second,
                consensus_timeout: Duration::from_secs(60),
                poll_interval: Duration::from_millis(10),
            }) {
                return ScenarioResult::Fail(format!("{}", e));
            }
        }

        ScenarioResult::Ok
    }
}

impl<TX, T> Scenario<'_, TestCase> for IrScenario<TX, T>
where
    TX: Transport,
    T: Target<TX> + HasTipHash + ConnectableTarget + CallRpc,
{
    fn new(args: &[String]) -> Result<Self, String> {
        let inner: GenericScenario<TX, T> = GenericScenario::new(args)?;

        let context = Self::build_program_context(&inner);
        log::info!("IR context: {:?}", context);

        let txos = Self::build_txos(&inner);
        let headers = Self::build_headers(&inner);

        Self::dump_context(context, txos, headers)?;

        #[cfg(any(feature = "oracle_netsplit", feature = "oracle_consensus"))]
        let second = Self::create_and_sync_second_target(args, &inner.target)?;

        Ok(Self {
            inner,
            #[cfg(any(feature = "oracle_netsplit", feature = "oracle_consensus"))]
            second,
        })
    }

    fn run(&mut self, testcase: TestCase) -> ScenarioResult {
        let hybrid_actions = self.build_hybrid_actions(&testcase);
        self.process_hybrid_actions(hybrid_actions);
        self.ping_connections();
        self.evaluate_oracles()
    }
}

fuzzamoto_main!(
    IrScenario::<fuzzamoto::connections::V1Transport, BitcoinCoreTarget>,
    TestCase
);
