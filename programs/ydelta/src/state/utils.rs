//! Small predicate helpers shared by the processors: clock access, seat
//! existence assertions, order-expiry and taker-permission gates.

use solana_program::{
    clock::Clock, entrypoint::ProgramResult, program_error::ProgramError, sysvar::Sysvar,
};

use hypertree::{is_not_nil, DataIndex, NIL};

use crate::program::YdeltaError;
use crate::require;

use super::resting_order::OrderType;

/// Returns the current unix timestamp from the Clock sysvar.
pub fn get_now_unix_ts() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}

/// Errors with `NoSeatClaimed` when the supplied seat index is `NIL`.
pub fn assert_already_has_seat(seat_index: DataIndex) -> ProgramResult {
    require!(
        is_not_nil!(seat_index),
        YdeltaError::NoSeatClaimed,
        "Trader has no claimed seat"
    )?;
    Ok(())
}

/// Errors with `OrderAlreadyExpired` when `last_valid_unix_ts` is past
/// `now`. Treats `0` as the "no expiry" sentinel.
pub fn assert_not_already_expired(last_valid_unix_ts: i64, now: i64) -> ProgramResult {
    if last_valid_unix_ts != 0 && last_valid_unix_ts < now {
        return Err(YdeltaError::OrderAlreadyExpired.into());
    }
    Ok(())
}

/// Errors with `PostOnlyWouldCross` if the order type is `PostOnly`,
/// which by definition is not allowed to take liquidity.
pub fn assert_can_take(order_type: OrderType) -> ProgramResult {
    require!(
        order_type != OrderType::PostOnly,
        YdeltaError::PostOnlyWouldCross,
        "PostOnly order would cross the book"
    )?;
    Ok(())
}
