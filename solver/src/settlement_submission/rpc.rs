use super::{
    gas_price_stream::gas_price_stream,
    retry::{CancelSender, SettlementSender},
    ESTIMATE_GAS_LIMIT_FACTOR,
};
use crate::{encoding::EncodedSettlement, pending_transactions::Fee, settlement::Settlement};
use anyhow::{Context, Result};
use contracts::GPv2Settlement;
use ethcontract::{dyns::DynTransport, Account, TransactionHash, Web3};
use futures::stream::StreamExt;
use gas_estimation::GasPriceEstimating;
use primitive_types::{H160, U256};
use std::{
    borrow::Cow,
    time::{Duration, Instant},
};
use transaction_retry::RetryResult;

// Submit a settlement to the contract, updating the transaction with gas prices if they increase.
#[allow(clippy::too_many_arguments)]
pub async fn submit(
    account: Account,
    contract: &GPv2Settlement,
    gas: &dyn GasPriceEstimating,
    target_confirm_time: Duration,
    gas_price_cap: f64,
    settlement: Settlement,
    gas_estimate: U256,
    private_network: Option<&shared::Web3>,
) -> Result<TransactionHash> {
    let address = account.address();
    let settlement: EncodedSettlement = settlement.into();

    let web3 = contract.raw_instance().web3();
    let nonce = web3
        .eth()
        .transaction_count(address, None)
        .await
        .context("failed to get transaction_count")?;
    let pending_gas_price =
        recover_gas_price_from_pending_transaction(&web3, &address, nonce).await?;

    // Account for some buffer in the gas limit in case racing state changes result in slightly more heavy computation at execution time
    let gas_limit = gas_estimate.to_f64_lossy() * ESTIMATE_GAS_LIMIT_FACTOR;

    let contract = match private_network {
        Some(rpc) => Cow::Owned(GPv2Settlement::at(rpc, contract.address())),
        None => Cow::Borrowed(contract),
    };
    let settlement_sender = SettlementSender {
        contract: &*contract,
        nonce,
        gas_limit,
        settlement,
        account,
    };
    // We never cancel.
    let cancel_future = std::future::pending::<CancelSender>();
    if let Some(gas_price) = pending_gas_price {
        tracing::info!(
            "detected existing pending transaction with gas price {}",
            gas_price
        );
    }

    // It is possible that there is a pending transaction we don't know about because the driver
    // got restarted while it was in progress. Sending a new transaction could fail in that case
    // because the gas price has not increased. So we make sure that the starting gas price is at
    // least high enough to accommodate. This isn't perfect because it's still possible that that
    // transaction gets mined first in which case our new transaction would fail with "nonce already
    // used".
    let pending_gas_price = pending_gas_price.map(|gas_price| {
        transaction_retry::gas_price_increase::minimum_increase(gas_price.to_f64_lossy())
    });
    let stream = gas_price_stream(
        Instant::now() + target_confirm_time,
        gas_price_cap,
        gas_limit,
        gas,
        pending_gas_price,
    )
    .boxed();

    match transaction_retry::retry(settlement_sender, cancel_future, stream).await {
        Some(RetryResult::Submitted(result)) => {
            tracing::info!("completed settlement submission");
            result.0.context("settlement transaction failed")
        }
        _ => unreachable!(),
    }
}

async fn recover_gas_price_from_pending_transaction(
    web3: &Web3<DynTransport>,
    address: &H160,
    nonce: U256,
) -> Result<Option<U256>> {
    let transactions = crate::pending_transactions::pending_transactions(web3.transport())
        .await
        .context("pending_transactions failed")?;
    let transaction = match transactions
        .iter()
        .find(|transaction| transaction.from == *address && transaction.nonce == nonce)
    {
        Some(transaction) => transaction,
        None => return Ok(None),
    };
    match transaction.fee {
        Fee::Legacy { gas_price } => Ok(Some(gas_price)),
        // vk: At time of writing we never create eip1559 transactions so this branch should not be
        // taken. Still, to be more future proof we return the priority fee.
        Fee::Eip1559 {
            max_priority_fee_per_gas,
            ..
        } => Ok(Some(max_priority_fee_per_gas)),
    }
}