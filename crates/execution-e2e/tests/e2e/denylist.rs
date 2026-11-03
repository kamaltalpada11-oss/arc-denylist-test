// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

//! E2E tests for the addresses denylist.
//!
//! Covers:
//! - to: denylisted → rejected
//! - from: denylisted → rejected
//! - addresses-exclusions: from denylisted but excluded → accepted

use alloy_primitives::{address, Address, TxKind, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_config::addresses_denylist::{
    AddressesDenylistConfig, DEFAULT_DENYLIST_ERC7201_BASE_SLOT, DENYLIST_ADDRESS_LOCALDEV,
};
use arc_execution_config::chainspec::ArcChainSpec;
use arc_execution_e2e::{chainspec::localdev_with_denylisted_addresses, ArcSetup, ArcTestNode};
use eyre::Result;
use jsonrpsee::core::client::Error as RpcClientError;
use reth_e2e_test_utils::wallet::Wallet;
use std::sync::Arc;

fn assert_denylisted_address_error(err: &eyre::Report, expected_addr: Address) {
    let rpc_err = err
        .downcast_ref::<RpcClientError>()
        .expect("Expected JSON-RPC client error");
    let RpcClientError::Call(call_err) = rpc_err else {
        panic!("Expected JSON-RPC call error, got: {rpc_err:?}");
    };
    let message = call_err.message().to_lowercase();
    assert!(
        message.contains("is denylisted"),
        "Expected denylist rejection in RPC error, got: {call_err:?}"
    );
    assert!(
        message.contains(&expected_addr.to_string().to_lowercase()),
        "Expected denylisted address {expected_addr} in RPC error, got: {call_err:?}"
    );
}

fn denylist_config_enabled(exclusions: Vec<Address>) -> AddressesDenylistConfig {
    AddressesDenylistConfig::new(
        DENYLIST_ADDRESS_LOCALDEV,
        DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
        exclusions,
    )
}

/// Launches a node with the given chain spec and denylist config, then signs and submits
/// a tx from the first wallet to `to`. Returns pool result (Err is PoolError when pool rejects).
async fn sign_and_submit_tx(
    chain_spec: Arc<ArcChainSpec>,
    addresses_denylist_config: AddressesDenylistConfig,
    to: Address,
) -> Result<(), eyre::Report> {
    let node = ArcTestNode::start(
        ArcSetup::new()
            .with_chain_spec(chain_spec.clone())
            .with_addresses_denylist_config(addresses_denylist_config),
    )
    .await?;
    let signer = node.wallet_signer(0)?;

    node.send_tx(
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            value: Some(U256::from(1)),
            to: Some(TxKind::Call(to)),
            gas: Some(26_000),
            input: TransactionInput::default(),
            ..Default::default()
        },
    )
    .await
    .map(|_| ())
}

/// Transaction to a denylisted address is rejected.
#[tokio::test]
async fn test_denylisted_to_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let denylisted_to = address!("0xdead000000000000000000000000000000000001");
    let chain_spec = localdev_with_denylisted_addresses(vec![denylisted_to]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new());
    let err = sign_and_submit_tx(chain_spec, addresses_denylist_config, denylisted_to)
        .await
        .expect_err("Expected RPC submission to reject tx to denylisted address");
    assert_denylisted_address_error(&err, denylisted_to);
    Ok(())
}

/// Transaction from a denylisted address is rejected.
#[tokio::test]
async fn test_denylisted_from_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let sender = Wallet::new(1).wallet_gen()[0].address();
    let chain_spec = localdev_with_denylisted_addresses(vec![sender]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new());
    let err = sign_and_submit_tx(chain_spec, addresses_denylist_config, Address::random())
        .await
        .expect_err("Expected RPC submission to reject tx from denylisted address");
    assert_denylisted_address_error(&err, sender);
    Ok(())
}

/// An address in `--arc.denylist.addresses-exclusions` is accepted despite being denylisted.
#[tokio::test]
async fn test_denylist_exclusion_accepts_from_denylisted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let sender = Wallet::new(1).wallet_gen()[0].address();
    let chain_spec = localdev_with_denylisted_addresses(vec![sender]);
    let addresses_denylist_config = denylist_config_enabled(vec![sender]);
    sign_and_submit_tx(chain_spec, addresses_denylist_config, Address::random())
        .await
        .expect("Expected RPC submission to accept tx when sender in addresses-exclusions");
    Ok(())
}

/// A payload built by a node whose txpool exclusion permits a denylisted recipient
/// is accepted and executed by a node enforcing the same denylist without that exclusion.
#[tokio::test]
async fn test_engine_new_payload_executes_denylisted_recipient_transaction() -> Result<()> {
    use alloy_rpc_types_engine::PayloadStatusEnum;
    use alloy_rpc_types_eth::BlockNumberOrTag;
    use super::helpers::payload::{forkchoice_state, next_payload_attributes};

    reth_tracing::init_test_tracing();

    let recipient = address!("0xdead000000000000000000000000000000000001");
    let chain_spec = localdev_with_denylisted_addresses(vec![recipient]);

    // Node A: the recipient is excluded from this node's txpool denylist.
    let node_a = ArcTestNode::start(
        ArcSetup::new()
            .with_chain_spec(chain_spec.clone())
            .with_addresses_denylist_config(denylist_config_enabled(vec![recipient])),
    )
    .await?;

    // Node B: same denylisted genesis, but no exclusions.
    let node_b = ArcTestNode::start(
        ArcSetup::new()
            .with_chain_spec(chain_spec)
            .with_addresses_denylist_config(denylist_config_enabled(Vec::new())),
    )
    .await?;

    let balance_before = node_b.balance(recipient, None).await?;

    // Submit through A's txpool. Its explicit exclusion should permit admission.
    let signer = node_a.wallet_signer(0)?;
    let tx_hash = node_a
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                value: Some(U256::from(1)),
                to: Some(TxKind::Call(recipient)),
                gas: Some(26_000),
                input: TransactionInput::default(),
                ..Default::default()
            },
        )
        .await?;

    // Build the payload on A without finalizing it there.
    let parent = node_a.get_block(BlockNumberOrTag::Latest).await?;
    let fcu_result = node_a
        .fork_choice_updated(
            forkchoice_state(parent.header.hash),
            Some(next_payload_attributes(parent.header.timestamp)),
        )
        .await?;
    let payload_id = fcu_result
        .payload_id
        .ok_or_else(|| eyre::eyre!("forkChoiceUpdated did not return a payload ID"))?;
    let payload = node_a.get_payload(payload_id).await?;

    assert_eq!(
        payload.transaction_count(),
        1,
        "the denylisted-recipient transaction must be included in A's payload"
    );

    // Submit the unchanged payload directly to B's Engine API.
    let payload_hash = payload.block_hash();
    let status = node_b.new_payload(payload.clone()).await?;
    assert!(
        matches!(&status, PayloadStatusEnum::Valid),
        "expected B to accept the payload, got {status:?}"
    );

    // Finalize the payload on B.
    let fcu_result = node_b
        .fork_choice_updated(forkchoice_state(payload_hash), None)
        .await?;
    assert!(
        matches!(&fcu_result.payload_status.status, PayloadStatusEnum::Valid),
        "expected B to finalize the payload, got {:?}",
        fcu_result.payload_status.status
    );

    let block = node_b.get_block(BlockNumberOrTag::Number(1)).await?;
    assert!(
        block.transactions.hashes().any(|hash| hash == tx_hash),
        "B's canonical block must contain the denylisted-recipient transaction"
    );

    let receipt = node_b.get_receipt(tx_hash).await?;
    assert!(receipt.status(), "the transaction must execute successfully");

    let balance_after = node_b.balance(recipient, None).await?;
    assert_eq!(
        balance_after - balance_before,
        U256::from(1),
        "the denylisted recipient must receive exactly 1 wei"
    );

    Ok(())
}
