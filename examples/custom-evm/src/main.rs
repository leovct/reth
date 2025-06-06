//! This example shows how to implement a node with a custom EVM

#![warn(unused_crate_dependencies)]

use alloy_evm::{eth::EthEvmContext, EvmFactory};
use alloy_genesis::Genesis;
use alloy_primitives::{address, Address, Bytes};
use reth::{
    builder::{components::ExecutorBuilder, BuilderContext, NodeBuilder},
    tasks::TaskManager,
};
use reth_ethereum::{
    chainspec::{Chain, ChainSpec},
    evm::{
        primitives::{Database, EvmEnv},
        revm::{
            context::{Cfg, Context, TxEnv},
            context_interface::{
                result::{EVMError, HaltReason},
                ContextTr,
            },
            handler::{EthPrecompiles, PrecompileProvider},
            inspector::{Inspector, NoOpInspector},
            interpreter::{interpreter::EthInterpreter, InputsImpl, InterpreterResult},
            precompile::{PrecompileFn, PrecompileOutput, PrecompileResult, Precompiles},
            primitives::hardfork::SpecId,
            MainBuilder, MainContext,
        },
        EthEvm, EthEvmConfig,
    },
    node::{
        api::{FullNodeTypes, NodeTypes},
        core::{args::RpcServerArgs, node_config::NodeConfig},
        node::EthereumAddOns,
        BasicBlockExecutorProvider, EthereumNode,
    },
    EthPrimitives,
};
use reth_tracing::{RethTracer, Tracer};
use std::sync::OnceLock;

/// Custom EVM configuration.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MyEvmFactory;

impl EvmFactory for MyEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>, EthInterpreter>> =
        EthEvm<DB, I, CustomPrecompiles>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let evm = Context::mainnet()
            .with_db(db)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env)
            .build_mainnet_with_inspector(NoOpInspector {})
            .with_precompiles(CustomPrecompiles::new());

        EthEvm::new(evm, false)
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        EthEvm::new(self.create_evm(db, input).into_inner().with_inspector(inspector), true)
    }
}

/// Builds a regular ethereum block executor that uses the custom EVM.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct MyExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for MyExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
{
    type EVM = EthEvmConfig<MyEvmFactory>;
    type Executor = BasicBlockExecutorProvider<Self::EVM>;

    async fn build_evm(
        self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<(Self::EVM, Self::Executor)> {
        let evm_config =
            EthEvmConfig::new_with_evm_factory(ctx.chain_spec(), MyEvmFactory::default());
        Ok((evm_config.clone(), BasicBlockExecutorProvider::new(evm_config)))
    }
}

/// A custom precompile that contains static precompiles.
#[derive(Clone)]
pub struct CustomPrecompiles {
    pub precompiles: EthPrecompiles,
}

impl CustomPrecompiles {
    /// Given a [`PrecompileProvider`] and cache for a specific precompiles, create a
    /// wrapper that can be used inside Evm.
    fn new() -> Self {
        Self { precompiles: EthPrecompiles::default() }
    }
}

/// Returns precompiles for Fjor spec.
pub fn prague_custom() -> &'static Precompiles {
    static INSTANCE: OnceLock<Precompiles> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let mut precompiles = Precompiles::prague().clone();
        // Custom precompile.
        precompiles.extend([(
            address!("0x0000000000000000000000000000000000000999"),
            |_, _| -> PrecompileResult {
                PrecompileResult::Ok(PrecompileOutput::new(0, Bytes::new()))
            } as PrecompileFn,
        )
            .into()]);
        precompiles
    })
}

impl<CTX: ContextTr> PrecompileProvider<CTX> for CustomPrecompiles {
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec) -> bool {
        let spec_id = spec.clone().into();
        if spec_id == SpecId::PRAGUE {
            self.precompiles = EthPrecompiles { precompiles: prague_custom(), spec: spec.into() }
        } else {
            PrecompileProvider::<CTX>::set_spec(&mut self.precompiles, spec);
        }
        true
    }

    fn run(
        &mut self,
        context: &mut CTX,
        address: &Address,
        inputs: &InputsImpl,
        is_static: bool,
        gas_limit: u64,
    ) -> Result<Option<Self::Output>, String> {
        self.precompiles.run(context, address, inputs, is_static, gas_limit)
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        self.precompiles.warm_addresses()
    }

    fn contains(&self, address: &Address) -> bool {
        self.precompiles.contains(address)
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let _guard = RethTracer::new().init()?;

    let tasks = TaskManager::current();

    // create a custom chain spec
    let spec = ChainSpec::builder()
        .chain(Chain::mainnet())
        .genesis(Genesis::default())
        .london_activated()
        .paris_activated()
        .shanghai_activated()
        .cancun_activated()
        .prague_activated()
        .build();

    let node_config =
        NodeConfig::test().with_rpc(RpcServerArgs::default().with_http()).with_chain(spec);

    let handle = NodeBuilder::new(node_config)
        .testing_node(tasks.executor())
        // configure the node with regular ethereum types
        .with_types::<EthereumNode>()
        // use default ethereum components but with our executor
        .with_components(EthereumNode::components().executor(MyExecutorBuilder::default()))
        .with_add_ons(EthereumAddOns::default())
        .launch()
        .await
        .unwrap();

    println!("Node started");

    handle.node_exit_future.await
}
