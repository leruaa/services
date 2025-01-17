use {
    super::SettlementSimulating,
    crate::{
        objective_value::Inputs,
        settlement::Settlement,
        solver::score_computation::ScoreCalculator,
    },
    anyhow::{anyhow, Result},
    ethcontract::Address,
    gas_estimation::GasPrice1559,
    num::{BigRational, FromPrimitive},
    shared::{external_prices::ExternalPrices, http_solver::model::Score},
};

pub async fn compute_score(
    settlement: &Settlement,
    settlement_simulator: &impl SettlementSimulating,
    score_calculator: &ScoreCalculator,
    gas_price: GasPrice1559,
    prices: &ExternalPrices,
    solver: &Address,
) -> Result<Score> {
    let gas_amount = settlement_simulator
        .estimate_gas(settlement.clone())
        .await
        .map(|gas_amount| {
            // Multiply by 0.9 to get more realistic gas amount.
            // This is because the gas estimation is not accurate enough and does not take
            // the EVM gas refund into account.
            gas_amount * 9 / 10
        })?;

    let inputs = Inputs::from_settlement(
        settlement,
        prices,
        BigRational::from_f64(gas_price.effective_gas_price()).ok_or_else(|| {
            anyhow!(
                "gas price {} fails conversion to BigRational",
                gas_price.effective_gas_price()
            )
        })?,
        &gas_amount,
    );
    let nmb_orders = settlement.trades().count();

    let score = score_calculator
        .calculate(&inputs, nmb_orders)
        .map(Score::Score);

    tracing::debug!(
        ?solver, ?score, objective_value = %inputs.objective_value(),
        "computed score",
    );

    score
}
