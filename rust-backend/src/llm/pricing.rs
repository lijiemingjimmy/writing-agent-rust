use crate::domain::{CostMicrousd, PriceSnapshot, Usage};

const TOKENS_PER_MILLION: u128 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PricingError {
    #[error("model cost overflows the supported microdollar range")]
    Overflow,
}

pub fn calculate_cost(usage: Usage, price: PriceSnapshot) -> Result<CostMicrousd, PricingError> {
    let input = independently_rounded_cost(usage.input_tokens, price.input_microusd_per_million)?;
    let output =
        independently_rounded_cost(usage.output_tokens, price.output_microusd_per_million)?;
    let total = input.checked_add(output).ok_or(PricingError::Overflow)?;
    let total = u64::try_from(total).map_err(|_| PricingError::Overflow)?;
    Ok(CostMicrousd(total))
}

fn independently_rounded_cost(tokens: u64, rate: u64) -> Result<u128, PricingError> {
    let numerator = u128::from(tokens)
        .checked_mul(u128::from(rate))
        .and_then(|value| value.checked_add(TOKENS_PER_MILLION - 1))
        .ok_or(PricingError::Overflow)?;
    Ok(numerator / TOKENS_PER_MILLION)
}
