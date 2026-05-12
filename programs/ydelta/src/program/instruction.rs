use num_enum::TryFromPrimitive;
use shank::ShankInstruction;

/// Instructions exposed by the yDelta program. Tags are contiguous;
/// new variants are appended at the next number.
#[repr(u8)]
#[derive(TryFromPrimitive, Debug, Copy, Clone, ShankInstruction, PartialEq, Eq)]
#[rustfmt::skip]
pub enum YdeltaInstruction {
    /// Create a `(debt_mint, collateral_mint)` lending market. Signer
    /// becomes `MarketFixed.admin`. Initialises both marginfi
    /// integration accounts (lender-side + borrower-side) via CPI.
    CreateMarket = 0,
    /// Allocate a user-owned `ClaimedSeat` for the signer in a market.
    /// Permissionless; auto-creates the signer's `UserAccount` PDA on
    /// first call.
    ClaimSeat = 1,
    /// Deposit debt-mint or collateral-mint atoms into the signer's
    /// seat. Atoms hop user_token → market staging vault → marginfi
    /// `lender_integration_account` (debt) or
    /// `borrower_integration_account` (collateral).
    Deposit = 2,
    /// Withdraw withdrawable shares from the signer's seat back to a
    /// wallet ATA. Atoms hop marginfi → market staging vault → wallet
    /// (with marginfi.withdraw health check).
    Withdraw = 3,
    /// Place a primary order (bid or ask). May trigger match-time
    /// cross with resting makers and, for Bids, a P2Pool fallback CPI
    /// to `marginfi.borrow` when the orderbook can't fill the
    /// residual. `OrderKind::SecondaryLoanSale` lists an existing Loan
    /// PDA at par via the same ix with one trailing account.
    PlaceOrder = 4,
    /// Cancel an open order owned by the signer. Releases encumbered
    /// shares back to the seat. `SecondaryLoanSale` cancels pass the
    /// Loan PDA so the processor can clear `has_resting_secondary_bid`.
    CancelOrder = 5,
    /// Update an open order owned by the signer. Implemented as
    /// cancel-and-replace; the new sequence number sends the order to
    /// the back of price-time priority. Secondary bids only allow
    /// `last_valid_unix_ts` and `flags` mutation in-place.
    UpdateOrder = 6,
    /// Promote a `MatchedLoan` queue entry into a `LoanFixed` PDA.
    /// Permissionless cranker. Cranker pays Loan-PDA rent.
    /// SECONDARY-flagged queue nodes mutate the existing loan in
    /// place; SECONDARY|SPLIT additionally allocates a sub-loan PDA.
    ProcessMatchedLoan = 7,
    /// Borrower repays principal + interest on an active `LoanFixed`.
    /// Wallet-lender path: atoms → market.lender_integration. Vault-
    /// lender path: atoms → vault.integration directly, with close-
    /// time bookkeeping (deployed-principal decrement, yield credit,
    /// curator/protocol fee split, PDA close). On full repay, the
    /// borrower's collateral returns to their seat as withdrawable
    /// shares.
    Repay = 8,
    /// Lender claims accrued atoms from a (partially or fully) repaid
    /// `LoanFixed`. Pure bookkeeping for wallet loans — atoms were
    /// deposited into `market.lender_integration` at repay time. On
    /// full settlement, closes the loan PDA and refunds rent to
    /// `loan.created_by` if `cranker_refund` matches.
    ClaimRepayment = 9,
    /// Permissionless utility: copy the canonical `ClaimedSeat`
    /// balances for `owner` onto `owner`'s `MarketPosition` mirror.
    /// Used to refresh stale mirrors after the canonical state moved
    /// without the owner signing (e.g. cranker finalised a match).
    SyncMarketPosition = 10,
    /// Allocate the per-mint `GlobalVault` PDA + initialise its
    /// marginfi integration account. Permissionless; first caller
    /// becomes `global_vault_admin`. One-shot per mint.
    CreateVault = 11,
    /// Insert a new `RiskProfile` into the vault's `risk_profiles`
    /// tree. Signer must equal `vault.global_vault_admin`. Stamps the
    /// profile's policy (`max_ltv_bps`, `max_term_seconds`,
    /// `allowed_market_max`), curator pubkey, and zeroed financial
    /// state. Realloc-grows the vault account for the new 512-byte
    /// block.
    CreateRiskProfile = 12,
    /// Depositor mints shares of a vault profile. Atoms hop wallet →
    /// vault staging → integration_account (into the pinned
    /// `lending_pool`). Updates `UserAccount.vault_positions` mirror.
    GlobalVaultDeposit = 13,
    /// Depositor burns shares to redeem atoms. Reverts if
    /// `idle_principal_atoms < atoms_out` — deployed liquidity is
    /// non-redeemable until repaid.
    GlobalVaultWithdraw = 14,
    /// Vault admin opens a market seat for a profile with a per-market
    /// `max_exposure_atoms` cap. Inserts the market-side `ClaimedSeat`
    /// (`owner_kind=Vault`, keyed by `(vault, profile_id)`) and stamps
    /// the cap on it. Bumps `RiskProfile.allowed_market_count`. Signer
    /// must equal `vault.global_vault_admin`. No atom migration at claim time.
    ClaimSeatForRiskProfile = 15,
    /// Curator places a vault Ask. Inserts a market-side `RestingOrder`
    /// sized at the vault seat's `max_exposure_atoms` cap, plus a
    /// vault-side `RiskProfileOrderRef` keyed by `(market, profile_id)` —
    /// at most one live order per profile per market. Open-ended:
    /// no per-seat encumbrance is taken; the profile's
    /// `idle_principal_atoms` pool backs the order, checked at match
    /// time. Signer must equal `profile.curator`.
    PlaceOrderForRiskProfile = 16,
    /// Curator cancels a vault order. Removes both the market-side
    /// `RestingOrder` and the vault-side `RiskProfileOrderRef`. Idempotent
    /// on missing. Signer must equal `profile.curator`.
    CancelOrderForRiskProfile = 17,
    /// Curator updates a vault order via cancel-and-replace; sequence
    /// number is renewed (back of price-time priority). Signer must
    /// equal `profile.curator`.
    UpdateOrderForRiskProfile = 18,
    /// Withdraws `RiskProfile.accumulated_curator_fee_atoms` to the
    /// curator's wallet ATA via marginfi.withdraw + SPL transfer.
    /// Signer must equal `profile.curator`. No-op when the accumulator
    /// is zero, so curators can poll.
    ClaimCuratorFee = 19,
    /// Permissionless time-gated keeper. Settles a loan past
    /// `matures_at + grace`. Liquidator pays up to `repay_atoms_max`
    /// of `outstanding_debt_atoms` and seizes pro-rata collateral.
    /// `repay_atoms_max = 0` (or ≥ outstanding) is a full repay and
    /// flips the loan to `Repaid`. Vault-funded loans route the
    /// liquidator's atoms directly to `vault.integration` and run the
    /// vault-side close bookkeeping inline.
    SettleMaturedLoan = 20,
    /// Permissionless LTV-gated keeper. Settles a loan whose current
    /// LTV breaches the maintenance threshold at oracle prices. Same
    /// atom flow as `SettleMaturedLoan`, plus an oracle-driven LTV
    /// gate (`get_required_quote_collateral_to_back_debt` against
    /// marginfi maint weights) and a tiered keeper bonus
    /// (`fee_config.liquidation_keeper_bps`). Surplus collateral
    /// returns to the borrower's seat; bad debt emits `BadDebtLog`.
    LiquidateLoan = 21,
    /// Updates per-market `FeeConfig` fields (protocol-fee floor,
    /// origination, curator split, liquidation keeper bonus, LTV
    /// buffer, grace period). Each `Some(_)` overrides; `None` leaves
    /// the field unchanged. Signer must equal `MarketFixed.admin`.
    /// Bps fields are bounded at 10_000.
    SetFeeConfig = 22,
    /// Drains `market.accumulated_protocol_fee_shares` to the admin's
    /// debt-token ATA via marginfi.withdraw + SPL transfer. Signer
    /// must equal `MarketFixed.admin`. No-op (returns Ok) when the
    /// accumulator is zero, so admins can poll safely.
    ProtocolFeeClaim = 23,
    /// Permissionless cranker. Realises a fully-repaid risk-profile
    /// loan: shifts the lender's seat shares from encumbered →
    /// withdrawable, sweeps the loan-body protocol fee onto the
    /// market accumulator, drains the realised atoms from
    /// `market.lender_integration_account` back into the GlobalVault's
    /// marginfi account, and closes the loan PDA. Updates the risk
    /// profile's `idle_principal_atoms` and the seat's `deployed_atoms`
    /// (per-market cap headroom). Companion to `ClaimRepayment` —
    /// wallet-lender path realises in pure bookkeeping, risk-profile
    /// path realises with atom drain.
    ClaimRepaymentForRiskProfile = 24,
    /// Initiate `MarketFixed.admin` transfer. Signer must equal the
    /// current `admin`; sets `pending_admin`. Two-step pattern guards
    /// against transferring to a non-controlled key.
    TransferMarketAdmin = 25,
    /// Finalise `MarketFixed.admin` transfer. Signer must equal
    /// `pending_admin`; promotes pending into admin and zeroes pending.
    AcceptMarketAdmin = 26,
    /// Initiate `GlobalVaultFixed.global_vault_admin` transfer. Signer must
    /// equal the current `global_vault_admin`; sets `pending_global_vault_admin`.
    TransferGlobalVaultAdmin = 27,
    /// Finalise `GlobalVaultFixed.global_vault_admin` transfer. Signer must
    /// equal `pending_global_vault_admin`.
    AcceptGlobalVaultAdmin = 28,
    /// Initiate per-profile `RiskProfile.curator` transfer. Signer
    /// must equal the current `curator`; sets `pending_curator`.
    /// Profile id passed in instruction data.
    TransferCurator = 29,
    /// Finalise per-profile `RiskProfile.curator` transfer. Signer
    /// must equal `pending_curator`. Profile id in ix data.
    AcceptCurator = 30,
    /// Sets `MarketFixed.is_paused` to the bool in instruction data.
    /// While paused, every state-mutating market ix rejects with
    /// `MarketPaused`; read-only ixs stay live. Signer must equal
    /// `MarketFixed.admin`.
    SetMarketPause = 31,
    /// One-shot. Allocates the `[b"global_config"]` PDA and stamps the
    /// deployer (signer) as the initial `protocol_admin`.
    CreateGlobalConfig = 32,
    /// Initiate `GlobalConfig.protocol_admin` transfer. Signer must
    /// equal the current `protocol_admin`.
    TransferProtocolAdmin = 33,
    /// Finalise `GlobalConfig.protocol_admin` transfer.
    AcceptProtocolAdmin = 34,
    /// Sets `GlobalConfig.is_paused`. While on, every state-mutating
    /// ix that takes the `global_config` account rejects with
    /// `GlobalPaused`. Signer must equal `GlobalConfig.protocol_admin`.
    SetGlobalPause = 35,
    /// Mutate fields of an existing `RiskProfile` (max_ltv_bps,
    /// max_term_seconds, allowed_market_max). Vault-admin gated.
    /// Existing market-side seats keep their stamped values until
    /// the next operation that touches them; the
    /// `SyncMarketSeatsForRiskProfile` cranker re-stamps them in
    /// bulk.
    UpdateRiskProfile = 36,
    /// Tear down a vault-owned `ClaimedSeat` in a market when it
    /// has no live state, decrementing the profile's
    /// `allowed_market_count`. Vault-admin gated. Counterpart to
    /// `ClaimSeatForRiskProfile` — closes the operational footgun where
    /// a profile that hits `allowed_market_max` would be locked
    /// out from new markets forever.
    ReleaseSeatForRiskProfile = 37,
    /// Permissionless cranker. Re-stamps every market-side
    /// `risk_profile_max_ltv_bps` cache for a given risk profile after the
    /// curator's `UpdateRiskProfile` has tightened (or loosened)
    /// `RiskProfile.max_ltv_bps`. Caller passes the (≤8) market
    /// accounts that must be re-stamped; the loader cross-checks
    /// each against `RiskProfile.active_markets`.
    SyncMarketSeatsForRiskProfile = 38,
    /// Borrower-initiated refinance: walks the asks tree and crosses
    /// every compatible wallet ask whose
    /// `rate_bps <= max_acceptable_rate_bps` AND
    /// `term_seconds >= (loan.matures_at - now)`. Each cross emits a
    /// fresh Fixed `MatchedLoan` queue node. Unfilled residual stays
    /// on the original P2Pool loan body. Full conversion closes the
    /// P2Pool PDA. Wallet ask-makers only.
    ConvertP2PoolToFixed = 39,
    /// Read-only simulation gate. Returns `Ok(())` iff the loan would
    /// fail marginfi's maintenance solvency check at current oracle
    /// prices (i.e. `liquidate_loan` would succeed). Errors with
    /// `LoanStillSolvent` otherwise. P2Pool loans gate against the
    /// live marginfi liability; Fixed loans against the accrued
    /// `outstanding_debt_atoms`. Designed for `simulateTransaction`
    /// callers (keepers, UIs) — same code path as `liquidate_loan`'s
    /// LTV check, so a successful sim guarantees a successful real
    /// call (modulo CPI side-effects).
    CheckLtvLiquidatable = 40,
    /// Read-only simulation gate. Returns `Ok(())` iff the loan is
    /// past `matures_at + grace_period_seconds` AND has live
    /// outstanding > 0 (i.e. `settle_matured_loan` would succeed).
    /// Errors with `LoanNotMatured` if pre-grace, or
    /// `InvalidArgument` if already settled.
    CheckMaturityLiquidatable = 41,
    /// Curator-gated. Update an already-claimed vault seat's
    /// `max_exposure_atoms` cap in place. Counterpart to
    /// `ClaimSeatForRiskProfile`'s one-shot stamp — lets the cranker
    /// keep the cap tracking the profile's live risk score × deposit
    /// base without releasing and re-claiming the seat. Rejects a new
    /// value below the seat's current `deployed_atoms`.
    SetSeatMaxExposureForRiskProfile = 42,
}

impl YdeltaInstruction {
    pub fn to_vec(&self) -> Vec<u8> {
        vec![*self as u8]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_tags_are_stable() {
        // Tags are contiguous 0..=last_tag. Every value in range
        // round-trips; everything beyond errors out.
        let last_tag: u8 = 42;
        for i in 0..=255u8 {
            match YdeltaInstruction::try_from(i) {
                Ok(ix) => {
                    assert!(i <= last_tag);
                    assert_eq!(ix as u8, i);
                }
                Err(_) => assert!(i > last_tag),
            }
        }
    }
}
