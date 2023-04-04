//! Protocol logic specific to processing ICS2 messages of type `MsgUpdateAnyClient`.

use crate::prelude::*;

use crate::core::ics02_client::events::{ClientMisbehaviour, UpdateClient};
use crate::core::ics02_client::msgs::update_client::MsgUpdateClient;
use crate::events::IbcEvent;

use crate::core::context::ContextError;

use crate::core::{ExecutionContext, ValidationContext};

pub(crate) fn validate<Ctx>(ctx: &Ctx, msg: MsgUpdateClient) -> Result<(), ContextError>
where
    Ctx: ValidationContext,
{
    let MsgUpdateClient {
        client_id,
        client_message,
        update_kind,
        signer: _,
    } = msg;

    // Read client type from the host chain store. The client should already exist.
    // Read client state from the host chain store.
    let client_state = ctx.client_state(&client_id)?;

    client_state.confirm_not_frozen()?;

    client_state
        .verify_client_message(ctx, &client_id, client_message, &update_kind)
        .map_err(ContextError::from)
}

pub(crate) fn execute<Ctx>(ctx: &mut Ctx, msg: MsgUpdateClient) -> Result<(), ContextError>
where
    Ctx: ExecutionContext,
{
    let MsgUpdateClient {
        client_id,
        client_message,
        update_kind,
        signer: _,
    } = msg;

    let client_state = ctx.client_state(&client_id)?;

    let found_misbehaviour = client_state.check_for_misbehaviour(
        ctx,
        &client_id,
        client_message.clone(),
        &update_kind,
    )?;

    if found_misbehaviour {
        client_state.update_state_on_misbehaviour(ctx, &client_id, client_message, &update_kind)?;

        let event = IbcEvent::ClientMisbehaviour(ClientMisbehaviour::new(
            client_id.clone(),
            client_state.client_type(),
        ));
        ctx.emit_ibc_event(IbcEvent::Message(event.event_type()));
        ctx.emit_ibc_event(event);
    } else {
        client_state.update_state(ctx, &client_id, client_message.clone(), &update_kind)?;

        // TODO: fix events stuff
        let consensus_height = client_state.latest_height();

        let event = IbcEvent::UpdateClient(UpdateClient::new(
            client_id,
            client_state.client_type(),
            consensus_height,
            vec![consensus_height],
            client_message,
        ));
        ctx.emit_ibc_event(IbcEvent::Message(event.event_type()));
        ctx.emit_ibc_event(event);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use core::str::FromStr;
    use ibc_proto::google::protobuf::Any;
    use test_log::test;

    use crate::clients::ics07_tendermint::client_type as tm_client_type;
    use crate::clients::ics07_tendermint::consensus_state::ConsensusState as TmConsensusState;
    use crate::clients::ics07_tendermint::header::Header as TmHeader;
    use crate::clients::ics07_tendermint::misbehaviour::Misbehaviour as TmMisbehaviour;
    use crate::core::ics02_client::client_state::ClientState;
    use crate::core::ics02_client::client_type::ClientType;
    use crate::core::ics02_client::consensus_state::downcast_consensus_state;
    use crate::core::ics02_client::handler::update_client::{execute, validate};
    use crate::core::ics02_client::msgs::update_client::{MsgUpdateClient, UpdateClientKind};
    use crate::core::ics24_host::identifier::{ChainId, ClientId};
    use crate::core::ValidationContext;
    use crate::events::{IbcEvent, IbcEventType};
    use crate::mock::client_state::client_type as mock_client_type;
    use crate::mock::client_state::MockClientState;
    use crate::mock::context::MockContext;
    use crate::mock::header::MockHeader;
    use crate::mock::host::{HostBlock, HostType};
    use crate::mock::misbehaviour::Misbehaviour as MockMisbehaviour;
    use crate::test_utils::get_dummy_account_id;
    use crate::timestamp::Timestamp;
    use crate::Height;
    use crate::{downcast, prelude::*};

    #[test]
    fn test_update_client_ok() {
        let client_id = ClientId::default();
        let signer = get_dummy_account_id();

        let timestamp = Timestamp::now();

        let mut ctx = MockContext::default().with_client(&client_id, Height::new(0, 42).unwrap());
        let height = Height::new(0, 46).unwrap();
        let msg = MsgUpdateClient {
            client_id,
            client_message: MockHeader::new(height).with_timestamp(timestamp).into(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer,
        };

        let res = validate(&ctx, msg.clone());

        assert!(res.is_ok(), "validation happy path");

        let res = execute(&mut ctx, msg.clone());
        assert!(res.is_ok(), "execution happy path");

        assert_eq!(
            ctx.client_state(&msg.client_id).unwrap(),
            MockClientState::new(MockHeader::new(height).with_timestamp(timestamp)).into_box()
        );
    }

    #[test]
    fn test_update_nonexisting_client() {
        let client_id = ClientId::from_str("mockclient1").unwrap();
        let signer = get_dummy_account_id();

        let ctx = MockContext::default().with_client(&client_id, Height::new(0, 42).unwrap());

        let msg = MsgUpdateClient {
            client_id: ClientId::from_str("nonexistingclient").unwrap(),
            client_message: MockHeader::new(Height::new(0, 46).unwrap()).into(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer,
        };

        let res = validate(&ctx, msg);

        assert!(res.is_err());
    }

    #[test]
    fn test_update_synthetic_tendermint_client_adjacent_ok() {
        let client_id = ClientId::new(tm_client_type(), 0).unwrap();
        let client_height = Height::new(1, 20).unwrap();
        let update_height = Height::new(1, 21).unwrap();
        let chain_id_b = ChainId::new("mockgaiaB".to_string(), 1);

        let mut ctx = MockContext::new(
            ChainId::new("mockgaiaA".to_string(), 1),
            HostType::Mock,
            5,
            Height::new(1, 1).unwrap(),
        )
        .with_client_parametrized_with_chain_id(
            chain_id_b.clone(),
            &client_id,
            client_height,
            Some(tm_client_type()), // The target host chain (B) is synthetic TM.
            Some(client_height),
        );

        let ctx_b = MockContext::new(chain_id_b, HostType::SyntheticTendermint, 5, update_height);

        let signer = get_dummy_account_id();

        let mut block = ctx_b.host_block(&update_height).unwrap().clone();
        block.set_trusted_height(client_height);

        let latest_header_height = block.height();
        let msg = MsgUpdateClient {
            client_id,
            client_message: block.into(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer,
        };

        let res = validate(&ctx, msg.clone());
        assert!(res.is_ok());

        let res = execute(&mut ctx, msg.clone());
        assert!(res.is_ok(), "result: {res:?}");

        let client_state = ctx.client_state(&msg.client_id).unwrap();
        assert!(client_state.confirm_not_frozen().is_ok());
        assert_eq!(client_state.latest_height(), latest_header_height);
    }

    #[test]
    fn test_update_synthetic_tendermint_client_non_adjacent_ok() {
        let client_id = ClientId::new(tm_client_type(), 0).unwrap();
        let client_height = Height::new(1, 20).unwrap();
        let update_height = Height::new(1, 21).unwrap();
        let chain_id_b = ChainId::new("mockgaiaB".to_string(), 1);

        let mut ctx = MockContext::new(
            ChainId::new("mockgaiaA".to_string(), 1),
            HostType::Mock,
            5,
            Height::new(1, 1).unwrap(),
        )
        .with_client_parametrized_history_with_chain_id(
            chain_id_b.clone(),
            &client_id,
            client_height,
            Some(tm_client_type()), // The target host chain (B) is synthetic TM.
            Some(client_height),
        );

        let ctx_b = MockContext::new(chain_id_b, HostType::SyntheticTendermint, 5, update_height);

        let signer = get_dummy_account_id();

        let mut block = ctx_b.host_block(&update_height).unwrap().clone();
        let trusted_height = client_height.clone().sub(1).unwrap();
        block.set_trusted_height(trusted_height);

        let latest_header_height = block.height();
        let msg = MsgUpdateClient {
            client_id,
            client_message: block.into(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer,
        };

        let res = validate(&ctx, msg.clone());
        assert!(res.is_ok());

        let res = execute(&mut ctx, msg.clone());
        assert!(res.is_ok(), "result: {res:?}");

        let client_state = ctx.client_state(&msg.client_id).unwrap();
        assert!(client_state.confirm_not_frozen().is_ok());
        assert_eq!(client_state.latest_height(), latest_header_height);
    }

    #[test]
    fn test_update_synthetic_tendermint_client_duplicate_ok() {
        let client_id = ClientId::new(tm_client_type(), 0).unwrap();
        let client_height = Height::new(1, 20).unwrap();

        let chain_start_height = Height::new(1, 11).unwrap();

        let mut ctx = MockContext::new(
            ChainId::new("mockgaiaA".to_string(), 1),
            HostType::Mock,
            5,
            chain_start_height,
        )
        .with_client_parametrized(
            &client_id,
            client_height,
            Some(tm_client_type()), // The target host chain (B) is synthetic TM.
            Some(client_height),
        );

        let ctx_b = MockContext::new(
            ChainId::new("mockgaiaB".to_string(), 1),
            HostType::SyntheticTendermint,
            5,
            client_height,
        );

        let signer = get_dummy_account_id();

        let block = ctx_b.host_block(&client_height).unwrap().clone();
        let block = match block {
            HostBlock::SyntheticTendermint(mut theader) => {
                let cons_state = ctx.latest_consensus_states(&client_id, &client_height);
                if let Some(tcs) = downcast_consensus_state::<TmConsensusState>(cons_state.as_ref())
                {
                    theader.light_block.signed_header.header.time = tcs.timestamp;
                    theader.trusted_height = Height::new(1, 11).unwrap();
                }
                HostBlock::SyntheticTendermint(theader)
            }
            _ => block,
        };

        let latest_header_height = block.height();
        let msg = MsgUpdateClient {
            client_id,
            client_message: block.into(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer,
        };

        let res = validate(&ctx, msg.clone());
        assert!(res.is_ok());

        let res = execute(&mut ctx, msg.clone());
        assert!(res.is_ok(), "result: {res:?}");

        let client_state = ctx.client_state(&msg.client_id).unwrap();
        assert!(client_state.confirm_not_frozen().is_ok());
        assert_eq!(client_state.latest_height(), latest_header_height);
        assert_eq!(client_state, ctx.latest_client_states(&msg.client_id));
    }

    #[test]
    fn test_update_synthetic_tendermint_client_lower_height() {
        let client_id = ClientId::new(tm_client_type(), 0).unwrap();
        let client_height = Height::new(1, 20).unwrap();

        let client_update_height = Height::new(1, 19).unwrap();

        let chain_start_height = Height::new(1, 11).unwrap();

        let ctx = MockContext::new(
            ChainId::new("mockgaiaA".to_string(), 1),
            HostType::Mock,
            5,
            chain_start_height,
        )
        .with_client_parametrized(
            &client_id,
            client_height,
            Some(tm_client_type()), // The target host chain (B) is synthetic TM.
            Some(client_height),
        );

        let ctx_b = MockContext::new(
            ChainId::new("mockgaiaB".to_string(), 1),
            HostType::SyntheticTendermint,
            5,
            client_height,
        );

        let signer = get_dummy_account_id();

        let block_ref = ctx_b.host_block(&client_update_height).unwrap();

        let msg = MsgUpdateClient {
            client_id,
            client_message: block_ref.clone().into(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer,
        };

        let res = validate(&ctx, msg);
        assert!(res.is_err());
    }

    #[test]
    fn test_update_client_events() {
        let client_id = ClientId::default();
        let signer = get_dummy_account_id();

        let timestamp = Timestamp::now();

        let mut ctx = MockContext::default().with_client(&client_id, Height::new(0, 42).unwrap());
        let height = Height::new(0, 46).unwrap();
        let header: Any = MockHeader::new(height).with_timestamp(timestamp).into();
        let msg = MsgUpdateClient {
            client_id: client_id.clone(),
            client_message: header.clone(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer,
        };

        let res = execute(&mut ctx, msg);
        assert!(res.is_ok());

        assert!(matches!(
            ctx.events[0],
            IbcEvent::Message(IbcEventType::UpdateClient)
        ));
        let update_client_event = downcast!(&ctx.events[1] => IbcEvent::UpdateClient).unwrap();

        assert_eq!(update_client_event.client_id(), &client_id);
        assert_eq!(update_client_event.client_type(), &mock_client_type());
        assert_eq!(update_client_event.consensus_height(), &height);
        assert_eq!(update_client_event.consensus_heights(), &vec![height]);
        assert_eq!(update_client_event.header(), &header);
    }

    fn ensure_misbehaviour(ctx: &MockContext, client_id: &ClientId, client_type: &ClientType) {
        let client_state = ctx.client_state(client_id).unwrap();

        assert!(client_state.confirm_not_frozen().is_err());

        // check events
        assert_eq!(ctx.events.len(), 2);
        assert!(matches!(
            ctx.events[0],
            IbcEvent::Message(IbcEventType::ClientMisbehaviour),
        ));
        let misbehaviour_client_event =
            downcast!(&ctx.events[1] => IbcEvent::ClientMisbehaviour).unwrap();
        assert_eq!(misbehaviour_client_event.client_id(), client_id);
        assert_eq!(misbehaviour_client_event.client_type(), client_type);
    }

    /// Tests misbehaviour handling for the mock client.
    /// Misbehaviour evidence consists of identical headers - mock misbehaviour handler considers it
    /// a valid proof of misbehaviour
    #[test]
    fn test_misbehaviour_client_ok() {
        let client_id = ClientId::default();
        let timestamp = Timestamp::now();
        let height = Height::new(0, 46).unwrap();
        let msg = MsgUpdateClient {
            client_id: client_id.clone(),
            client_message: MockMisbehaviour {
                client_id: client_id.clone(),
                header1: MockHeader::new(height).with_timestamp(timestamp),
                header2: MockHeader::new(height).with_timestamp(timestamp),
            }
            .into(),
            update_kind: UpdateClientKind::UpdateHeader,
            signer: get_dummy_account_id(),
        };

        let mut ctx = MockContext::default().with_client(&client_id, Height::new(0, 42).unwrap());

        let res = validate(&ctx, msg.clone());
        assert!(res.is_ok());
        let res = execute(&mut ctx, msg);
        assert!(res.is_ok());

        ensure_misbehaviour(&ctx, &client_id, &mock_client_type());
    }

    /// Tests misbehaviour handling failure for a non-existent client
    #[test]
    fn test_misbehaviour_nonexisting_client() {
        let client_id = ClientId::from_str("mockclient1").unwrap();
        let height = Height::new(0, 46).unwrap();
        let msg = MsgUpdateClient {
            client_id: ClientId::from_str("nonexistingclient").unwrap(),
            client_message: MockMisbehaviour {
                client_id: client_id.clone(),
                header1: MockHeader::new(height),
                header2: MockHeader::new(height),
            }
            .into(),
            update_kind: UpdateClientKind::Misbehaviour,
            signer: get_dummy_account_id(),
        };

        let ctx = MockContext::default().with_client(&client_id, Height::new(0, 42).unwrap());
        let res = validate(&ctx, msg);
        assert!(res.is_err());
    }

    /// Tests misbehaviour handling for the synthetic Tendermint client.
    /// Misbehaviour evidence consists of equivocal headers.
    #[test]
    fn test_misbehaviour_synthetic_tendermint_equivocation() {
        let client_id = ClientId::new(tm_client_type(), 0).unwrap();
        let client_height = Height::new(1, 20).unwrap();
        let misbehaviour_height = Height::new(1, 21).unwrap();
        let chain_id_b = ChainId::new("mockgaiaB".to_string(), 1);

        // Create a mock context for chain-A with a synthetic tendermint light client for chain-B
        let mut ctx_a = MockContext::new(
            ChainId::new("mockgaiaA".to_string(), 1),
            HostType::Mock,
            5,
            Height::new(1, 1).unwrap(),
        )
        .with_client_parametrized_with_chain_id(
            chain_id_b.clone(),
            &client_id,
            client_height,
            Some(tm_client_type()),
            Some(client_height),
        );

        // Create a mock context for chain-B
        let ctx_b = MockContext::new(
            chain_id_b.clone(),
            HostType::SyntheticTendermint,
            5,
            misbehaviour_height,
        );

        // Get chain-B's header at `misbehaviour_height`
        let header1: TmHeader = {
            let mut block = ctx_b.host_block(&misbehaviour_height).unwrap().clone();
            block.set_trusted_height(client_height);
            block.try_into_tm_block().unwrap().into()
        };

        // Generate an equivocal header for chain-B at `misbehaviour_height`
        let header2 = {
            let mut tm_block = HostBlock::generate_tm_block(
                chain_id_b,
                misbehaviour_height.revision_height(),
                Timestamp::now(),
            );
            tm_block.trusted_height = client_height;
            tm_block.into()
        };

        let msg = MsgUpdateClient {
            client_id: client_id.clone(),
            client_message: TmMisbehaviour::new(client_id.clone(), header1, header2)
                .unwrap()
                .into(),
            update_kind: UpdateClientKind::Misbehaviour,
            signer: get_dummy_account_id(),
        };

        let res = validate(&ctx_a, msg.clone());
        assert!(res.is_ok());
        let res = execute(&mut ctx_a, msg);
        assert!(res.is_ok());
        ensure_misbehaviour(&ctx_a, &client_id, &tm_client_type());
    }

    #[test]
    fn test_misbehaviour_synthetic_tendermint_bft_time() {
        let client_id = ClientId::new(tm_client_type(), 0).unwrap();
        let client_height = Height::new(1, 20).unwrap();
        let misbehaviour_height = Height::new(1, 21).unwrap();
        let chain_id_b = ChainId::new("mockgaiaB".to_string(), 1);

        // Create a mock context for chain-A with a synthetic tendermint light client for chain-B
        let mut ctx_a = MockContext::new(
            ChainId::new("mockgaiaA".to_string(), 1),
            HostType::Mock,
            5,
            Height::new(1, 1).unwrap(),
        )
        .with_client_parametrized_with_chain_id(
            chain_id_b.clone(),
            &client_id,
            client_height,
            Some(tm_client_type()),
            Some(client_height),
        );

        // Generate `header1` for chain-B
        let header1 = {
            let mut tm_block = HostBlock::generate_tm_block(
                chain_id_b.clone(),
                misbehaviour_height.revision_height(),
                Timestamp::now(),
            );
            tm_block.trusted_height = client_height;
            tm_block
        };

        // Generate `header2` for chain-B which is identical to `header1` but with a conflicting
        // timestamp
        let header2 = {
            let timestamp =
                Timestamp::from_nanoseconds(Timestamp::now().nanoseconds() + 1_000_000_000)
                    .unwrap();
            let mut tm_block = HostBlock::generate_tm_block(
                chain_id_b,
                misbehaviour_height.revision_height(),
                timestamp,
            );
            tm_block.trusted_height = client_height;
            tm_block
        };

        let msg = MsgUpdateClient {
            client_id: client_id.clone(),
            client_message: TmMisbehaviour::new(client_id.clone(), header1.into(), header2.into())
                .unwrap()
                .into(),
            update_kind: UpdateClientKind::Misbehaviour,
            signer: get_dummy_account_id(),
        };

        let res = validate(&ctx_a, msg.clone());
        assert!(res.is_ok());
        let res = execute(&mut ctx_a, msg);
        assert!(res.is_ok());
        ensure_misbehaviour(&ctx_a, &client_id, &tm_client_type());
    }
}
