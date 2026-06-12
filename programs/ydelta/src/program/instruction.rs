//! Instruction tag enum. The first byte of every transaction's
//! `instruction_data` is one of these variants; the entrypoint dispatches
//! to the matching processor in [`crate::program::processor`]. Tags are
//! a contiguous `0..=40` range — see `tests::instruction_tags_are_contiguous`.

use num_enum::TryFromPrimitive;
use shank::ShankInstruction;

#[repr(u8)]
#[derive(TryFromPrimitive, Debug, Copy, Clone, ShankInstruction, PartialEq, Eq)]
#[rustfmt::skip]
pub enum YdeltaInstruction {
    /// Initialize a `MarketFixed` PDA for a `(debt_mint, collateral_mint)`
    /// pair, plus its lender / borrower marginfi integration accounts.
    CreateMarket = 0,

    /// Insert a per-user `ClaimedSeat` into a market's seat tree.
    /// Auto-creates the signer's `UserAccountFixed` PDA on first call.
    ClaimSeat = 1,

    /// Move atoms from a user wallet ATA into the signer's market seat
    /// (debt-side or collateral-side, selected by the token-account mint).
    Deposit = 2,

    /// Burn seat shares back to a wallet ATA. Either a fixed share count
    /// or a "drain all on this side" sentinel.
    Withdraw = 3,

    /// Submit a borrower IOC bid. The matching engine crosses resting
    /// vault asks first; any unfilled residual either falls through to a
    /// P2Pool marginfi-backed loan or drops (controlled by the OB_ONLY flag).
    PlaceOrder = 4,

    /// Cranker step: promote a queued `MatchedLoan` node into a
    /// `LoanFixed` PDA. For vault-funded matches, also runs the
    /// vault-side migration of atoms from `vault.integration_account` into
    /// the market's `lender_integration_account`.
    ProcessMatchedLoan = 5,

    /// Borrower-side repay against a `LoanFixed`. Pays atoms from the
    /// borrower's debt ATA. Full repay closes the loan PDA and folds
    /// the close-out accounting into the lender vault profile in-place.
    Repay = 6,

    /// Permissionless: copy a market `ClaimedSeat`'s balances onto the
    /// matching `MarketPosition` row in the owner's `UserAccountFixed`.
    SyncMarketPosition = 7,

    /// One-shot per mint: create the `GlobalVaultFixed` PDA and its
    /// marginfi integration account. First caller becomes `global_vault_admin`.
    CreateVault = 8,

    /// Vault-admin gated: append a `SubVault` to the vault. Stamps
    /// curator + LTV cap (optional, defaults to marginfi-derived) +
    /// max term. Auto-assigns a monotonic `sub_vault_id` starting at 1.
    CreateSubVault = 9,

    /// Lender deposits atoms into a sub-vault, minting profile shares.
    GlobalVaultDeposit = 10,

    /// Burn profile shares to redeem atoms. Rejected when the burn would
    /// drop idle below 0 or break the per-profile idle gate.
    GlobalVaultWithdraw = 11,

    /// Curator-gated: rest an unbounded vault ask on a market. Auto-
    /// creates the per-(profile, market) `ClaimedSeat` and the
    /// vault-side `SubVaultOrderRef` on first call.
    PlaceOrderForSubVault = 12,

    /// Curator-gated: remove a vault ask (clears the resting order +
    /// the vault-side `SubVaultOrderRef`).
    CancelOrderForSubVault = 13,

    /// Curator-gated: cancel-and-replace a vault ask. Same as cancel
    /// followed by place in a single tx, with a fresh order sequence.
    UpdateOrderForSubVault = 14,

    /// Curator-gated: withdraw the profile's accrued
    /// `accumulated_curator_fee_atoms` to the curator's wallet.
    ClaimCuratorFee = 15,

    /// Permissionless past `matures_at + grace`. Keeper repays up to
    /// `repay_atoms_max` of the live debt and seizes proportional
    /// collateral. Full repay closes the loan PDA in-place.
    SettleMaturedLoan = 16,

    /// Permissionless when current LTV breaches the maintenance threshold
    /// at marginfi maint weights. Same close shape as `SettleMaturedLoan`.
    LiquidateLoan = 17,

    /// Market-admin gated: update `MarketFixed.fee_config` fields. Each
    /// `Some` field overrides; `None` leaves the existing value.
    SetFeeConfig = 18,

    /// Market-admin gated: drain `market.accumulated_protocol_fee_shares`
    /// to the admin's debt ATA via marginfi.withdraw.
    ProtocolFeeClaim = 19,

    /// Permissionless: sweep a profile's `pending_claim_atoms` from the
    /// per-market `lender_marginfi_account` back into the vault's own
    /// `integration_account` (one-way profile → vault sweeper).
    ClaimRepaymentForSubVault = 20,

    /// Initiate market-admin handoff (two-step transfer). Signer must
    /// equal the current admin; sets `pending_admin`.
    TransferMarketAdmin = 21,

    /// Finalize market-admin handoff. Signer must equal `pending_admin`.
    AcceptMarketAdmin = 22,

    /// Initiate vault-admin handoff. Signer must equal the current vault admin.
    TransferGlobalVaultAdmin = 23,

    /// Finalize vault-admin handoff.
    AcceptGlobalVaultAdmin = 24,

    /// Initiate per-profile curator handoff. Signer must equal the
    /// current curator. Profile id passed in instruction data.
    TransferCurator = 25,

    /// Finalize per-profile curator handoff.
    AcceptCurator = 26,

    /// Market-admin gated: set `MarketFixed.is_paused`. Paused markets
    /// reject every state-mutating market ix.
    SetMarketPause = 27,

    /// One-shot: create the singleton `GlobalConfig` PDA. Deployer
    /// (signer) becomes `protocol_admin`.
    CreateGlobalConfig = 28,

    /// Initiate protocol-admin handoff.
    TransferProtocolAdmin = 29,

    /// Finalize protocol-admin handoff.
    AcceptProtocolAdmin = 30,

    /// Protocol-admin gated: set `GlobalConfig.is_paused`. Globally pauses
    /// every state-mutating ix that takes the global config account.
    SetGlobalPause = 31,

    /// Vault-admin gated: update mutable `SubVault` policy fields
    /// (`max_ltv_bps`, `max_term_seconds`). Rejected on sunset profiles.
    UpdateSubVault = 32,

    /// Borrower-initiated upgrade of a P2Pool (variable-rate) loan into
    /// one or more Fixed loans. The matcher crosses compatible vault asks
    /// and each cross emits a `MatchedLoan` for the cranker to promote.
    ConvertP2PoolToFixed = 33,

    /// Read-only simulation. Returns `Ok(())` iff the loan would fail
    /// the maintenance-LTV solvency check at current oracle prices
    /// (i.e. `LiquidateLoan` would succeed).
    CheckLtvLiquidatable = 34,

    /// Read-only simulation. Returns `Ok(())` iff the loan is past
    /// `matures_at + grace_period_seconds` AND has live outstanding > 0.
    CheckMaturityLiquidatable = 35,

    /// Vault-admin gated: set `GlobalVaultFixed.is_paused`. Paused vaults
    /// reject vault-scoped state-mutating ixs.
    SetVaultPause = 36,

    /// Vault-admin gated: remove a `SubVault` from the vault tree.
    /// Requires `is_sunset == 1` — admin takes responsibility for the
    /// state via the sunset wind-down workflow before invoking remove.
    RemoveSubVault = 37,

    /// Vault-admin gated: flip `profile.is_sunset = 1`. Disables new
    /// deposits / orders / order updates / matches against the profile,
    /// while withdrawals and curator-side cleanups remain enabled.
    SunsetSubVault = 38,

    /// Vault-admin gated: flip `profile.is_sunset = 0`. Reverses
    /// `SunsetSubVault` (admin escape hatch).
    ResumeSubVault = 39,

    /// Vault-admin gated: force-cancel a vault ask. Only allowed on
    /// sunset profiles (during wind-down) — non-sunset profiles can only
    /// be cancelled by the curator via `CancelOrderForSubVault`.
    AdminCancelSubVaultOrder = 40,
}

impl YdeltaInstruction {
    /// Borsh-prefix shorthand: returns `vec![tag_byte]`, ready for the
    /// caller to append serialized `Params`.
    pub fn to_vec(&self) -> Vec<u8> {
        vec![*self as u8]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_tags_are_contiguous() {
        let last_tag: u8 = 40;
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
