//! Self-match prevention.
//!
//! A literal self-match is not expressible: the only resting orders
//! are vault risk-profile asks (seat owner = the vault PDA,
//! `OWNER_KIND_RISK_PROFILE`); the only takers are borrower IOC bids
//! (seat owner = a borrower wallet, `OWNER_KIND_USER`). A maker seat
//! and a taker seat can therefore never be the same `ClaimedSeat`, so
//! the `SelfMatchForbidden` guard in `match_order` is unreachable from
//! any instruction.
//!
//! The guard is kept in the matching engine as defense-in-depth
//! against future state corruption, but there is no integration test
//! that can exercise it.
