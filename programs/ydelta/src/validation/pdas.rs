use solana_program::pubkey::Pubkey;

pub const MARKET_SIGNER_SEED: &[u8] = b"market_signer";

pub const MARGINFI_LENDER_ACCOUNT_SEED: &[u8] = b"marginfi_account";

pub const MARGINFI_BORROWER_ACCOUNT_SEED: &[u8] = b"borrower_marginfi_account";

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

pub fn get_marginfi_account_address(market: &Pubkey) -> (Pubkey, u8) {
    get_lender_integration_account_address(market)
}
