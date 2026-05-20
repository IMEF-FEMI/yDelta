//! PDA seeds and derivation helpers for market authorities and
//! integration accounts.

use solana_program::pubkey::Pubkey;

/// Seed prefix for the per-market authority PDA. Final seeds:
/// `[MARKET_SIGNER_SEED, market.key]`.
pub const MARKET_SIGNER_SEED: &[u8] = b"market_signer";

/// Seed prefix for the lender-side marginfi-account PDA. Final seeds:
/// `[MARGINFI_LENDER_ACCOUNT_SEED, market.key]`. The byte string
/// `b"marginfi_account"` must not change or every lender-side PDA
/// address would shift.
pub const MARGINFI_LENDER_ACCOUNT_SEED: &[u8] = b"marginfi_account";

/// Seed prefix for the borrower-side marginfi-account PDA. Final seeds:
/// `[MARGINFI_BORROWER_ACCOUNT_SEED, market.key]`.
pub const MARGINFI_BORROWER_ACCOUNT_SEED: &[u8] = b"borrower_marginfi_account";

/// Alias for the lender-side PDA seed. New callers should use
/// `MARGINFI_LENDER_ACCOUNT_SEED` for clarity.
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

/// Macro alias for the lender-side seeds. New callers should use
/// `marginfi_lender_account_seeds_with_bump!`.
#[macro_export]
macro_rules! marginfi_account_seeds_with_bump {
    ( $market:expr, $bump:expr ) => {
        $crate::marginfi_lender_account_seeds_with_bump!($market, $bump)
    };
}

pub fn get_market_signer_address(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MARKET_SIGNER_SEED, market.as_ref()], &crate::ID)
}

pub fn get_lender_integration_account_address(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MARGINFI_LENDER_ACCOUNT_SEED, market.as_ref()], &crate::ID)
}

pub fn get_borrower_integration_account_address(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[MARGINFI_BORROWER_ACCOUNT_SEED, market.as_ref()],
        &crate::ID,
    )
}

/// Alias for the lender-side PDA derivation. Returns the same address
/// as `get_lender_integration_account_address`; new callers should use
/// that name directly.
pub fn get_marginfi_account_address(market: &Pubkey) -> (Pubkey, u8) {
    get_lender_integration_account_address(market)
}
