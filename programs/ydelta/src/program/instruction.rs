use num_enum::TryFromPrimitive;
use shank::ShankInstruction;

/// Instructions exposed by the yDelta program. Tags are a clean
/// contiguous `0..N` with no gaps; new variants are appended at the
/// next number.
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
    /// (with marginfi.withdraw health check). The `withdraw_all` flag
    /// drains the seat's full withdrawable balance on the selected side.
    Withdraw = 3,
    /// Place a borrower IOC bid. Crosses the resting vault risk-profile
    /// asks; any residual either fires a P2Pool fallback CPI to
    /// `marginfi.borrow` or drops. The bid never rests on the book.
    PlaceOrder = 4,
    /// Promote a `MatchedLoan` queue entry into a `LoanFixed` PDA.
    /// Permissionless cranker. Cranker pays Loan-PDA rent.
    ProcessMatchedLoan = 5,
    /// Borrower repays principal + interest on an active `LoanFixed`.
    /// Every loan's lender is a vault risk profile: repaid atoms route
    /// to `vault.integration` directly, with close-time bookkeeping
    /// (deployed-principal decrement, yield credit, curator/protocol
    /// fee split, PDA close). For a P2Pool loan the repayment is made
    /// against the borrower marginfi liability; `full_repay` retires
    /// that liability to zero. On full repay, the borrower's collateral
    /// returns to their seat as withdrawable shares.
    Repay = 6,
    /// Permissionless utility: copy the canonical `ClaimedSeat`
    /// balances for `owner` onto `owner`'s `MarketPosition` mirror.
    /// Used to refresh stale mirrors after the canonical state moved
    /// without the owner signing (e.g. cranker finalised a match).
    SyncMarketPosition = 7,
    /// Allocate the per-mint `GlobalVault` PDA + initialise its
    /// marginfi integration account. Permissionless; first caller
    /// becomes `global_vault_admin`. One-shot per mint.
    CreateVault = 8,
    /// Insert a new `RiskProfile` into the vault's `risk_profiles`
    /// tree. Signer must equal `vault.global_vault_admin`. Stamps the
    /// profile's policy (`max_ltv_bps`, `max_term_seconds`), curator
    /// pubkey, and zeroed financial state. Realloc-grows the vault
    /// account for the new 512-byte block.
    CreateRiskProfile = 9,
    /// Depositor mints shares of a vault profile. Atoms hop wallet →
    /// vault staging → integration_account (into the pinned
    /// `lending_pool`). Updates `UserAccount.vault_positions` mirror.
    GlobalVaultDeposit = 10,
    /// Depositor burns shares to redeem atoms. Reverts if
    /// `idle_principal_atoms < atoms_out` — deployed liquidity is
    /// non-redeemable until repaid.
    GlobalVaultWithdraw = 11,
    /// Curator places a vault Ask. Auto-creates the vault-owned
    /// `ClaimedSeat` (`owner_kind=Vault`, keyed by `(vault, profile_id)`)
    /// on the first order in a market, then inserts an unbounded
    /// market-side `RestingOrder` plus a vault-side `RiskProfileOrderRef`
    /// keyed by `(market, profile_id)` — at most one live order per
    /// profile per market. The ask is unbounded ("quote all idle"):
    /// no per-seat encumbrance is taken and the resting order carries
    /// no fixed principal; each cross is capped by the profile's live
    /// idle pool at match time. A profile may quote any market sharing
    /// the vault's mint. Signer must equal `profile.curator`.
    PlaceOrderForRiskProfile = 12,
    /// Curator cancels a vault order. Removes both the market-side
    /// `RestingOrder` and the vault-side `RiskProfileOrderRef`. Idempotent
    /// on missing. Signer must equal `profile.curator`.
    CancelOrderForRiskProfile = 13,
    /// Curator updates a vault order via cancel-and-replace; sequence
    /// number is renewed (back of price-time priority). Signer must
    /// equal `profile.curator`.
    UpdateOrderForRiskProfile = 14,
    /// Withdraws `RiskProfile.accumulated_curator_fee_atoms` to the
    /// curator's wallet ATA via marginfi.withdraw + SPL transfer.
    /// Signer must equal `profile.curator`. No-op when the accumulator
    /// is zero, so curators can poll.
    ClaimCuratorFee = 15,
    /// Permissionless time-gated keeper. Settles a loan past
    /// `matures_at + grace`. Liquidator pays up to `repay_atoms_max`
    /// of `outstanding_debt_atoms` and seizes pro-rata collateral.
    /// `repay_atoms_max = 0` (or ≥ outstanding) is a full repay and
    /// flips the loan to `Repaid`. The liquidator's atoms route
    /// directly to `vault.integration` and run the vault-side close
    /// bookkeeping inline.
    SettleMaturedLoan = 16,
    /// Permissionless LTV-gated keeper. Settles a loan whose current
    /// LTV breaches the maintenance threshold at oracle prices. Same
    /// atom flow as `SettleMaturedLoan`, plus an oracle-driven LTV
    /// gate (`get_required_quote_collateral_to_back_debt` against
    /// marginfi maint weights) and a tiered keeper bonus
    /// (`fee_config.liquidation_keeper_bps`). Surplus collateral
    /// returns to the borrower's seat; bad debt emits `BadDebtLog`.
    LiquidateLoan = 17,
    /// Updates per-market `FeeConfig` fields (protocol-fee floor,
    /// origination, curator split, liquidation keeper bonus, LTV
    /// buffer, grace period). Each `Some(_)` overrides; `None` leaves
    /// the field unchanged. Signer must equal `MarketFixed.admin`.
    /// Bps fields are bounded at 10_000. No pause gate (admin-only
    /// header mutation; runs during the paused setup window).
    SetFeeConfig = 18,
    /// Drains `market.accumulated_protocol_fee_shares` to the admin's
    /// debt-token ATA via marginfi.withdraw + SPL transfer. Signer
    /// must equal `MarketFixed.admin`. No-op (returns Ok) when the
    /// accumulator is zero, so admins can poll safely.
    ProtocolFeeClaim = 19,
    /// Permissionless cranker. Realises a fully-repaid risk-profile
    /// loan: shifts the lender's seat shares from encumbered →
    /// withdrawable, sweeps the loan-body protocol fee onto the
    /// market accumulator, drains the realised atoms from
    /// `market.lender_integration_account` back into the GlobalVault's
    /// marginfi account, and closes the loan PDA. Updates the risk
    /// profile's `idle_principal_atoms` and `deployed_principal_atoms`.
    ClaimRepaymentForRiskProfile = 20,
    /// Initiate `MarketFixed.admin` transfer. Signer must equal the
    /// current `admin`; sets `pending_admin`. Two-step pattern guards
    /// against transferring to a non-controlled key.
    TransferMarketAdmin = 21,
    /// Finalise `MarketFixed.admin` transfer. Signer must equal
    /// `pending_admin`; promotes pending into admin and zeroes pending.
    AcceptMarketAdmin = 22,
    /// Initiate `GlobalVaultFixed.global_vault_admin` transfer. Signer must
    /// equal the current `global_vault_admin`; sets `pending_global_vault_admin`.
    TransferGlobalVaultAdmin = 23,
    /// Finalise `GlobalVaultFixed.global_vault_admin` transfer. Signer must
    /// equal `pending_global_vault_admin`.
    AcceptGlobalVaultAdmin = 24,
    /// Initiate per-profile `RiskProfile.curator` transfer. Signer
    /// must equal the current `curator`; sets `pending_curator`.
    /// Profile id passed in instruction data.
    TransferCurator = 25,
    /// Finalise per-profile `RiskProfile.curator` transfer. Signer
    /// must equal `pending_curator`. Profile id in ix data.
    AcceptCurator = 26,
    /// Sets `MarketFixed.is_paused` to the bool in instruction data.
    /// While paused, every state-mutating market ix rejects with
    /// `MarketPaused`; read-only ixs stay live. Signer must equal
    /// `MarketFixed.admin`.
    SetMarketPause = 27,
    /// One-shot. Allocates the `[b"global_config"]` PDA and stamps the
    /// deployer (signer) as the initial `protocol_admin`.
    CreateGlobalConfig = 28,
    /// Initiate `GlobalConfig.protocol_admin` transfer. Signer must
    /// equal the current `protocol_admin`.
    TransferProtocolAdmin = 29,
    /// Finalise `GlobalConfig.protocol_admin` transfer.
    AcceptProtocolAdmin = 30,
    /// Sets `GlobalConfig.is_paused`. While on, every state-mutating
    /// ix that takes the `global_config` account rejects with
    /// `GlobalPaused`. Signer must equal `GlobalConfig.protocol_admin`.
    SetGlobalPause = 31,
    /// Mutate fields of an existing `RiskProfile` (max_ltv_bps,
    /// max_term_seconds). Vault-admin gated. The matching engine reads
    /// `max_ltv_bps` live from the `RiskProfile` at match time, so a
    /// change here takes effect immediately with no seat re-sync.
    UpdateRiskProfile = 32,
    /// Borrower-initiated upgrade of a P2Pool (variable-rate) loan to
    /// fixed rate: walks the asks tree and crosses every compatible
    /// vault risk-profile ask whose `rate_bps <= max_acceptable_rate_bps`
    /// AND `term_seconds >= (loan.matures_at - now)`. Each cross emits a
    /// fresh Fixed `MatchedLoan` queue node. Unfilled residual stays on
    /// the original P2Pool loan body. Full conversion closes the P2Pool
    /// PDA only when the post-CPI live marginfi liability is zero.
    ConvertP2PoolToFixed = 33,
    /// Read-only simulation gate. Returns `Ok(())` iff the loan would
    /// fail marginfi's maintenance solvency check at current oracle
    /// prices (i.e. `liquidate_loan` would succeed). Errors with
    /// `LoanStillSolvent` otherwise. P2Pool loans gate against the
    /// live marginfi liability; Fixed loans against the accrued
    /// `outstanding_debt_atoms`. Designed for `simulateTransaction`
    /// callers (keepers, UIs) — same code path as `liquidate_loan`'s
    /// LTV check, so a successful sim guarantees a successful real
    /// call (modulo CPI side-effects).
    CheckLtvLiquidatable = 34,
    /// Read-only simulation gate. Returns `Ok(())` iff the loan is
    /// past `matures_at + grace_period_seconds` AND has live
    /// outstanding > 0 (i.e. `settle_matured_loan` would succeed).
    /// Errors with `LoanNotMatured` if pre-grace, or
    /// `InvalidArgument` if already settled.
    CheckMaturityLiquidatable = 35,
    /// Sets `GlobalVaultFixed.is_paused` to the bool in instruction
    /// data. While paused, every vault-scoped state-mutating ix
    /// (deposit / withdraw, place / cancel / update risk-profile order,
    /// claim repayment / curator fee, create / remove / update risk
    /// profile) rejects with `VaultPaused`; the two-step vault-admin
    /// transfer ixs stay live so admin control can be recovered while
    /// paused. Signer must equal `GlobalVaultFixed.global_vault_admin`.
    SetVaultPause = 36,
    /// Remove a `RiskProfile` from the vault's `risk_profiles` tree.
    /// Vault-admin-gated. The profile's `profile_id` is passed in
    /// instruction data. **Hard precondition**: the profile must be
    /// empty — `total_shares`, `total_assets_atoms`,
    /// `total_principal_atoms`, `deployed_principal_atoms`,
    /// `encumbered_in_orders_atoms`, and `accumulated_curator_fee_atoms`
    /// must all be zero — otherwise the ix rejects with
    /// `VaultProfileNotEmpty`. The freed 512-byte block returns to the
    /// profile free list and `risk_profile_count` decrements. The
    /// vault's `next_profile_id` is NOT decremented: the id is retired
    /// forever, so historical references (closed loans, off-chain
    /// indexers) stay unambiguous.
    RemoveRiskProfile = 37,
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
    fn instruction_tags_are_contiguous() {
        // Tags form a contiguous `0..=last_tag` range with no gaps:
        // every tag in range round-trips through `try_from`, and every
        // value beyond `last_tag` is rejected.
        let last_tag: u8 = 37;
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
