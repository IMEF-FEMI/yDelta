use num_enum::TryFromPrimitive;
use shank::ShankInstruction;

#[repr(u8)]
#[derive(TryFromPrimitive, Debug, Copy, Clone, ShankInstruction, PartialEq, Eq)]
#[rustfmt::skip]
pub enum YdeltaInstruction {

    CreateMarket = 0,

    ClaimSeat = 1,

    Deposit = 2,

    Withdraw = 3,

    PlaceOrder = 4,

    ProcessMatchedLoan = 5,

    Repay = 6,

    SyncMarketPosition = 7,

    CreateVault = 8,

    CreateRiskProfile = 9,

    GlobalVaultDeposit = 10,

    GlobalVaultWithdraw = 11,

    PlaceOrderForRiskProfile = 12,

    CancelOrderForRiskProfile = 13,

    UpdateOrderForRiskProfile = 14,

    ClaimCuratorFee = 15,

    SettleMaturedLoan = 16,

    LiquidateLoan = 17,

    SetFeeConfig = 18,

    ProtocolFeeClaim = 19,

    ClaimRepaymentForRiskProfile = 20,

    TransferMarketAdmin = 21,

    AcceptMarketAdmin = 22,

    TransferGlobalVaultAdmin = 23,

    AcceptGlobalVaultAdmin = 24,

    TransferCurator = 25,

    AcceptCurator = 26,

    SetMarketPause = 27,

    CreateGlobalConfig = 28,

    TransferProtocolAdmin = 29,

    AcceptProtocolAdmin = 30,

    SetGlobalPause = 31,

    UpdateRiskProfile = 32,

    ConvertP2PoolToFixed = 33,

    CheckLtvLiquidatable = 34,

    CheckMaturityLiquidatable = 35,

    SetVaultPause = 36,

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
