use solana_program::{
    clock::Clock, entrypoint::ProgramResult, program_error::ProgramError, sysvar::Sysvar,
};

use hypertree::{is_not_nil, DataIndex, NIL};

use crate::program::YdeltaError;
use crate::require;

use super::resting_order::OrderType;

pub fn get_now_unix_ts() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}

pub fn assert_already_has_seat(seat_index: DataIndex) -> ProgramResult {
    require!(
        is_not_nil!(seat_index),
        YdeltaError::NoSeatClaimed,
        "Trader has no claimed seat"
    )?;
    Ok(())
}

pub fn assert_not_already_expired(last_valid_unix_ts: i64, now: i64) -> ProgramResult {
    if last_valid_unix_ts != 0 && last_valid_unix_ts < now {
        return Err(YdeltaError::OrderAlreadyExpired.into());
    }
    Ok(())
}

pub fn assert_can_take(order_type: OrderType) -> ProgramResult {
    require!(
        order_type != OrderType::PostOnly,
        YdeltaError::PostOnlyWouldCross,
        "PostOnly order would cross the book"
    )?;
    Ok(())
}
