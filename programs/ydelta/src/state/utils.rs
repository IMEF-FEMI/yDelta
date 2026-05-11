use solana_program::{
    clock::Clock, entrypoint::ProgramResult, program_error::ProgramError, sysvar::Sysvar,
};

use hypertree::{is_not_nil, DataIndex, NIL};

use crate::program::YdeltaError;
use crate::require;

use super::resting_order::OrderType;

/// Current wall-clock unix timestamp from the Clock sysvar. yDelta uses
/// unix-ts (not slot) for order expiry because terms are measured in
/// days. Surfacing the Clock failure (rather than swallowing it as
/// `0`) avoids silent expiry-check bypass — a `0` timestamp would
/// make every `last_valid_unix_ts > 0` order look "not yet expired"
/// regardless of true wall-clock.
pub fn get_now_unix_ts() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}

/// Returns `Err(NoSeatClaimed)` if `seat_index == NIL`.
pub fn assert_already_has_seat(seat_index: DataIndex) -> ProgramResult {
    require!(
        is_not_nil!(seat_index),
        YdeltaError::NoSeatClaimed,
        "Trader has no claimed seat"
    )?;
    Ok(())
}

/// Returns `Err(OrderAlreadyExpired)` if `last_valid_unix_ts != 0` and is
/// already in the past at `now`.
pub fn assert_not_already_expired(last_valid_unix_ts: i64, now: i64) -> ProgramResult {
    if last_valid_unix_ts != 0 && last_valid_unix_ts < now {
        return Err(YdeltaError::OrderAlreadyExpired.into());
    }
    Ok(())
}

/// Returns `Err(PostOnlyWouldCross)` if the order type is `PostOnly` — used
/// at the matching boundary when a `PostOnly` order would otherwise take.
pub fn assert_can_take(order_type: OrderType) -> ProgramResult {
    require!(
        order_type != OrderType::PostOnly,
        YdeltaError::PostOnlyWouldCross,
        "PostOnly order would cross the book"
    )?;
    Ok(())
}
