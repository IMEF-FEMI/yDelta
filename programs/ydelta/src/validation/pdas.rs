//! PDA seed constants, address derivations, and `invoke_signed`
//! seed-with-bump macros for the per-market PDAs the program owns
//! (market signer authority + lender / borrower marginfi integration
//! accounts). All addresses are keyed off the `market` pubkey.

use solana_program::pubkey::Pubkey;

/// Seed prefix for the per-market signer PDA. Signer authority for the
/// market's vault ATAs and marginfi integration accounts.
pub const MARKET_SIGNER_SEED: &[u8] = b"market_signer";

/// Seed prefix for the per-market lender-side marginfi integration
/// account (holds asset shares from the debt bank).
pub const MARGINFI_LENDER_ACCOUNT_SEED: &[u8] = b"marginfi_account";

/// Seed prefix for the per-market borrower-side marginfi integration
/// account (holds collateral asset shares + debt liability shares for
/// P2Pool loans).
pub const MARGINFI_BORROWER_ACCOUNT_SEED: &[u8] = b"borrower_marginfi_account";

/// Back-compat alias for [`MARGINFI_LENDER_ACCOUNT_SEED`]. New code
/// should use the explicit lender/borrower constant.
pub const MARGINFI_ACCOUNT_SEED: &[u8] = MARGINFI_LENDER_ACCOUNT_SEED;

#[macro_export]
macro_rules! market_signer_seeds_with_bump {
    ( $market:expr, $bump:expr ) => {
        &[&[
            $crate::validation::MARKET_SIGNER_SEED,
            $market.as_ref(),
            &[$bump],
        ]]
    };
}

#[macro_export]
macro_rules! marginfi_lender_account_seeds_with_bump {
    ( $market:expr, $bump:expr ) => {
        &[&[
            $crate::validation::MARGINFI_LENDER_ACCOUNT_SEED,
            $market.as_ref(),
            &[$bump],
        ]]
    };
}

#[macro_export]
macro_rules! marginfi_borrower_account_seeds_with_bump {
    ( $market:expr, $bump:expr ) => {
        &[&[
            $crate::validation::MARGINFI_BORROWER_ACCOUNT_SEED,
            $market.as_ref(),
            &[$bump],
        ]]
    };
}

#[macro_export]
macro_rules! marginfi_account_seeds_with_bump {
    ( $market:expr, $bump:expr ) => {
        $crate::marginfi_lender_account_seeds_with_bump!($market, $bump)
    };
}

/// Derive `(market_signer, bump)` for the given market.
pub fn get_market_signer_address(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MARKET_SIGNER_SEED, market.as_ref()], &crate::ID)
}

/// Derive `(lender_integration_account, bump)` for the given market.
pub fn get_lender_integration_account_address(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MARGINFI_LENDER_ACCOUNT_SEED, market.as_ref()], &crate::ID)
}

/// Derive `(borrower_integration_account, bump)` for the given market.
pub fn get_borrower_integration_account_address(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[MARGINFI_BORROWER_ACCOUNT_SEED, market.as_ref()],
        &crate::ID,
    )
}

/// Back-compat alias for [`get_lender_integration_account_address`].
pub fn get_marginfi_account_address(market: &Pubkey) -> (Pubkey, u8) {
    get_lender_integration_account_address(market)
}
