pub mod mock;
pub mod tendermint;

use alloc::collections::VecDeque;
use core::fmt::Debug;
use core::ops::Add;
use core::time::Duration;

use ibc::core::client::context::consensus_state::ConsensusState;
use ibc::core::client::types::Height;
use ibc::core::host::types::identifiers::ChainId;
use ibc::core::primitives::prelude::*;
use ibc::core::primitives::Timestamp;
use ibc::primitives::proto::Any;
use typed_builder::TypedBuilder;

pub use self::mock::MockHost;
pub use self::tendermint::TendermintHost;
use crate::testapp::ibc::clients::{AnyClientState, AnyConsensusState};
use crate::testapp::ibc::core::types::DEFAULT_BLOCK_TIME_SECS;
use crate::utils::year_2023;

#[derive(Debug, TypedBuilder)]
pub struct HostParams {
    #[builder(default = ChainId::new("mockgaia-0").expect("Never fails"))]
    pub chain_id: ChainId,
    #[builder(default = Duration::from_secs(DEFAULT_BLOCK_TIME_SECS))]
    pub block_time: Duration,
    #[builder(default = year_2023())]
    pub genesis_timestamp: Timestamp,
}

pub type HostClientState<H> = <H as TestHost>::ClientState;
pub type HostBlock<H> = <H as TestHost>::Block;
pub type HostBlockParams<H> = <H as TestHost>::BlockParams;
pub type HostLightClientParams<H> = <H as TestHost>::LightClientParams;
pub type HostHeader<H> = <HostBlock<H> as TestBlock>::Header;
pub type HostConsensusState<H> = <HostHeader<H> as TestHeader>::ConsensusState;

/// TestHost is a trait that defines the interface for a host blockchain.
pub trait TestHost: Debug + Sized {
    /// The type of block produced by the host.
    type Block: TestBlock;

    /// The type of client state produced by the host.
    type ClientState: Into<AnyClientState> + Debug;

    /// The type of block parameters to produce a block
    type BlockParams: Debug + Default;

    /// The type of light client parameters to produce a light client state
    type LightClientParams: Debug + Default;

    /// Build a new host with the given parameters.
    fn build(params: HostParams) -> Self;

    fn history(&self) -> &VecDeque<Self::Block>;

    /// The chain identifier of the host.
    fn chain_id(&self) -> &ChainId;

    /// The block production time of the host.
    fn block_time(&self) -> Duration;

    /// The genesis timestamp of the host.
    fn genesis_timestamp(&self) -> Timestamp;

    /// Returns true if the host chain has no blocks.
    fn is_empty(&self) -> bool {
        self.history().is_empty()
    }

    /// The latest height of the host chain.
    fn latest_height(&self) -> Height {
        self.latest_block().height()
    }

    /// The latest block of the host chain.
    fn latest_block(&self) -> Self::Block {
        self.history().back().cloned().expect("no error")
    }

    /// Get the block at the given height.
    fn get_block(&self, target_height: &Height) -> Option<Self::Block> {
        self.history()
            .get(target_height.revision_height() as usize - 1)
            .cloned() // indexed from 1
    }

    /// Add a block to the host chain.
    fn push_block(&mut self, block: Self::Block);

    /// Prune blocks until the given height.
    fn prune_block_till(&mut self, height: &Height);

    /// Triggers the advancing of the host chain, by extending the history of blocks (or headers).
    fn advance_block(&mut self, commitment_root: Vec<u8>, params: &Self::BlockParams) {
        let (height, timestamp) = if self.is_empty() {
            (1, self.genesis_timestamp())
        } else {
            let latest_block = self.latest_block();

            (
                TestBlock::height(&latest_block)
                    .increment()
                    .revision_height(),
                TestBlock::timestamp(&latest_block)
                    .add(self.block_time())
                    .expect("Never fails"),
            )
        };

        let new_block = self.generate_block(commitment_root, height, timestamp, params);

        // History is not full yet.
        self.push_block(new_block);
    }

    /// Generate a block at the given height and timestamp, using the provided parameters.
    fn generate_block(
        &self,
        commitment_root: Vec<u8>,
        height: u64,
        timestamp: Timestamp,
        params: &Self::BlockParams,
    ) -> Self::Block;

    /// Generate a client state using the block at the given height and the provided parameters.
    fn generate_client_state(
        &self,
        latest_height: &Height,
        params: &Self::LightClientParams,
    ) -> Self::ClientState;

    fn validate(&self) -> Result<(), String> {
        // Check that headers in the history are in sequential order.
        let latest_height = self.latest_height();
        let mut current_height = Height::min(latest_height.revision_number());

        while current_height <= latest_height {
            if current_height != self.get_block(&current_height).expect("no error").height() {
                return Err("block height does not match".to_owned());
            }
            current_height = current_height.increment();
        }
        Ok(())
    }
}

/// TestBlock is a trait that defines the interface for a block produced by a host blockchain.
pub trait TestBlock: Clone + Debug {
    /// The type of header can be extracted from the block.
    type Header: TestHeader + From<Self>;

    /// The height of the block.
    fn height(&self) -> Height;

    /// The timestamp of the block.
    fn timestamp(&self) -> Timestamp;

    /// Extract the header from the block.
    fn into_header(self) -> Self::Header {
        self.into()
    }
}

/// TestHeader is a trait that defines the interface for a header produced by a host blockchain.
pub trait TestHeader: Clone + Debug + Into<Any> {
    /// The type of consensus state can be extracted from the header.
    type ConsensusState: ConsensusState + Into<AnyConsensusState> + From<Self> + Clone + Debug;

    /// The height of the block, as recorded in the header.
    fn height(&self) -> Height;

    /// The timestamp of the block, as recorded in the header.
    fn timestamp(&self) -> Timestamp;

    /// Extract the consensus state from the header.
    fn into_consensus_state(self) -> Self::ConsensusState {
        Self::ConsensusState::from(self)
    }
}
