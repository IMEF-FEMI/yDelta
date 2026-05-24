use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::discriminator::{
    IX_LENDING_ACCOUNT_BORROW, IX_LENDING_ACCOUNT_DEPOSIT, IX_LENDING_ACCOUNT_REPAY,
    IX_LENDING_ACCOUNT_WITHDRAW, IX_MARGINFI_ACCOUNT_INITIALIZE,
};

pub struct DepositAccounts {
    pub group: Pubkey,
    pub marginfi_account: Pubkey,
    pub authority: Pubkey,
    pub bank: Pubkey,
    pub signer_token_account: Pubkey,
    pub liquidity_vault: Pubkey,
    pub token_program: Pubkey,
}

pub fn deposit_ix(
    accounts: &DepositAccounts,
    amount: u64,
    deposit_up_to_limit: Option<bool>,
    remaining_accounts: &[AccountMeta],
) -> Instruction {
    let mut data = IX_LENDING_ACCOUNT_DEPOSIT.to_vec();
    amount.serialize(&mut data).unwrap();
    deposit_up_to_limit.serialize(&mut data).unwrap();

    let mut metas = vec![
        AccountMeta::new_readonly(accounts.group, false),
        AccountMeta::new(accounts.marginfi_account, false),
        AccountMeta::new_readonly(accounts.authority, true),
        AccountMeta::new(accounts.bank, false),
        AccountMeta::new(accounts.signer_token_account, false),
        AccountMeta::new(accounts.liquidity_vault, false),
        AccountMeta::new_readonly(accounts.token_program, false),
    ];
    metas.extend_from_slice(remaining_accounts);

    Instruction {
        program_id: crate::ID,
        accounts: metas,
        data,
    }
}

pub struct WithdrawAccounts {
    pub group: Pubkey,
    pub marginfi_account: Pubkey,
    pub authority: Pubkey,
    pub bank: Pubkey,
    pub destination_token_account: Pubkey,
    pub bank_liquidity_vault_authority: Pubkey,
    pub liquidity_vault: Pubkey,
    pub token_program: Pubkey,
}

pub fn withdraw_ix(
    accounts: &WithdrawAccounts,
    amount: u64,
    withdraw_all: Option<bool>,
    remaining_accounts: &[AccountMeta],
) -> Instruction {
    let mut data = IX_LENDING_ACCOUNT_WITHDRAW.to_vec();
    amount.serialize(&mut data).unwrap();
    withdraw_all.serialize(&mut data).unwrap();

    let mut metas = vec![
        AccountMeta::new_readonly(accounts.group, false),
        AccountMeta::new(accounts.marginfi_account, false),
        AccountMeta::new_readonly(accounts.authority, true),
        AccountMeta::new(accounts.bank, false),
        AccountMeta::new(accounts.destination_token_account, false),
        AccountMeta::new_readonly(accounts.bank_liquidity_vault_authority, false),
        AccountMeta::new(accounts.liquidity_vault, false),
        AccountMeta::new_readonly(accounts.token_program, false),
    ];
    metas.extend_from_slice(remaining_accounts);

    Instruction {
        program_id: crate::ID,
        accounts: metas,
        data,
    }
}

pub struct BorrowAccounts {
    pub group: Pubkey,
    pub marginfi_account: Pubkey,
    pub authority: Pubkey,
    pub bank: Pubkey,
    pub destination_token_account: Pubkey,
    pub bank_liquidity_vault_authority: Pubkey,
    pub liquidity_vault: Pubkey,
    pub token_program: Pubkey,
}

pub fn borrow_ix(
    accounts: &BorrowAccounts,
    amount: u64,
    remaining_accounts: &[AccountMeta],
) -> Instruction {
    let mut data = IX_LENDING_ACCOUNT_BORROW.to_vec();
    amount.serialize(&mut data).unwrap();

    let mut metas = vec![
        AccountMeta::new_readonly(accounts.group, false),
        AccountMeta::new(accounts.marginfi_account, false),
        AccountMeta::new_readonly(accounts.authority, true),
        AccountMeta::new(accounts.bank, false),
        AccountMeta::new(accounts.destination_token_account, false),
        AccountMeta::new_readonly(accounts.bank_liquidity_vault_authority, false),
        AccountMeta::new(accounts.liquidity_vault, false),
        AccountMeta::new_readonly(accounts.token_program, false),
    ];
    metas.extend_from_slice(remaining_accounts);

    Instruction {
        program_id: crate::ID,
        accounts: metas,
        data,
    }
}

pub struct RepayAccounts {
    pub group: Pubkey,
    pub marginfi_account: Pubkey,
    pub authority: Pubkey,
    pub bank: Pubkey,
    pub signer_token_account: Pubkey,
    pub liquidity_vault: Pubkey,
    pub token_program: Pubkey,
}

pub fn repay_ix(
    accounts: &RepayAccounts,
    amount: u64,
    repay_all: Option<bool>,
    remaining_accounts: &[AccountMeta],
) -> Instruction {
    let mut data = IX_LENDING_ACCOUNT_REPAY.to_vec();
    amount.serialize(&mut data).unwrap();
    repay_all.serialize(&mut data).unwrap();

    let mut metas = vec![
        AccountMeta::new_readonly(accounts.group, false),
        AccountMeta::new(accounts.marginfi_account, false),
        AccountMeta::new_readonly(accounts.authority, true),
        AccountMeta::new(accounts.bank, false),
        AccountMeta::new(accounts.signer_token_account, false),
        AccountMeta::new(accounts.liquidity_vault, false),
        AccountMeta::new_readonly(accounts.token_program, false),
    ];
    metas.extend_from_slice(remaining_accounts);

    Instruction {
        program_id: crate::ID,
        accounts: metas,
        data,
    }
}

pub struct InitializeMarginfiAccounts {
    pub marginfi_group: Pubkey,
    pub marginfi_account: Pubkey,
    pub authority: Pubkey,
    pub fee_payer: Pubkey,
    pub system_program: Pubkey,
}

pub fn initialize_marginfi_account_ix(accounts: &InitializeMarginfiAccounts) -> Instruction {
    let data = IX_MARGINFI_ACCOUNT_INITIALIZE.to_vec();
    Instruction {
        program_id: crate::ID,
        accounts: vec![
            AccountMeta::new_readonly(accounts.marginfi_group, false),
            AccountMeta::new(accounts.marginfi_account, true),
            AccountMeta::new_readonly(accounts.authority, true),
            AccountMeta::new(accounts.fee_payer, true),
            AccountMeta::new_readonly(accounts.system_program, false),
        ],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(seed: u8) -> Pubkey {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        Pubkey::new_from_array(bytes)
    }

    fn deposit_accounts() -> DepositAccounts {
        DepositAccounts {
            group: pk(1),
            marginfi_account: pk(2),
            authority: pk(3),
            bank: pk(4),
            signer_token_account: pk(5),
            liquidity_vault: pk(6),
            token_program: pk(7),
        }
    }

    fn withdraw_accounts() -> WithdrawAccounts {
        WithdrawAccounts {
            group: pk(1),
            marginfi_account: pk(2),
            authority: pk(3),
            bank: pk(4),
            destination_token_account: pk(5),
            bank_liquidity_vault_authority: pk(6),
            liquidity_vault: pk(7),
            token_program: pk(8),
        }
    }

    fn borrow_accounts() -> BorrowAccounts {
        BorrowAccounts {
            group: pk(1),
            marginfi_account: pk(2),
            authority: pk(3),
            bank: pk(4),
            destination_token_account: pk(5),
            bank_liquidity_vault_authority: pk(6),
            liquidity_vault: pk(7),
            token_program: pk(8),
        }
    }

    fn repay_accounts() -> RepayAccounts {
        RepayAccounts {
            group: pk(1),
            marginfi_account: pk(2),
            authority: pk(3),
            bank: pk(4),
            signer_token_account: pk(5),
            liquidity_vault: pk(6),
            token_program: pk(7),
        }
    }

    #[test]
    fn deposit_ix_data_is_disc_then_amount_then_option_none() {
        let ix = deposit_ix(&deposit_accounts(), 1_000_000, None, &[]);
        assert_eq!(&ix.data[..8], &IX_LENDING_ACCOUNT_DEPOSIT);
        assert_eq!(&ix.data[8..16], &1_000_000u64.to_le_bytes());

        assert_eq!(ix.data[16], 0);
        assert_eq!(ix.data.len(), 17);
        assert_eq!(ix.accounts.len(), 7);
        assert_eq!(ix.program_id, crate::ID);
    }

    #[test]
    fn withdraw_ix_data_is_disc_then_amount_then_option_some_true() {
        let ix = withdraw_ix(&withdraw_accounts(), 500, Some(true), &[]);
        assert_eq!(&ix.data[..8], &IX_LENDING_ACCOUNT_WITHDRAW);
        assert_eq!(&ix.data[8..16], &500u64.to_le_bytes());

        assert_eq!(ix.data[16], 1);
        assert_eq!(ix.data[17], 1);
        assert_eq!(ix.data.len(), 18);
        assert_eq!(ix.accounts.len(), 8);
    }

    #[test]
    fn borrow_ix_has_no_option_arg() {
        let ix = borrow_ix(&borrow_accounts(), 42, &[]);
        assert_eq!(&ix.data[..8], &IX_LENDING_ACCOUNT_BORROW);
        assert_eq!(&ix.data[8..16], &42u64.to_le_bytes());
        assert_eq!(ix.data.len(), 16);
    }

    #[test]
    fn repay_ix_serialises_repay_all() {
        let ix = repay_ix(&repay_accounts(), 10, Some(false), &[]);
        assert_eq!(&ix.data[..8], &IX_LENDING_ACCOUNT_REPAY);
        assert_eq!(ix.data[16], 1);
        assert_eq!(ix.data[17], 0);
    }

    #[test]
    fn withdraw_appends_remaining_accounts_for_health_check() {
        let bank0 = pk(100);
        let oracle0 = pk(101);
        let bank1 = pk(102);
        let oracle1 = pk(103);
        let remaining = vec![
            AccountMeta::new_readonly(bank0, false),
            AccountMeta::new_readonly(oracle0, false),
            AccountMeta::new_readonly(bank1, false),
            AccountMeta::new_readonly(oracle1, false),
        ];
        let ix = withdraw_ix(&withdraw_accounts(), 100, None, &remaining);

        assert_eq!(ix.accounts.len(), 12);

        assert_eq!(ix.accounts[8].pubkey, bank0);
        assert_eq!(ix.accounts[9].pubkey, oracle0);
        assert_eq!(ix.accounts[10].pubkey, bank1);
        assert_eq!(ix.accounts[11].pubkey, oracle1);
    }

    #[test]
    fn borrow_appends_remaining_accounts_for_health_check() {
        let bank = pk(50);
        let oracle = pk(51);
        let remaining = vec![
            AccountMeta::new_readonly(bank, false),
            AccountMeta::new_readonly(oracle, false),
        ];
        let ix = borrow_ix(&borrow_accounts(), 1_000, &remaining);
        assert_eq!(ix.accounts.len(), 10);
        assert_eq!(ix.accounts[8].pubkey, bank);
        assert_eq!(ix.accounts[9].pubkey, oracle);
    }

    #[test]
    fn deposit_with_remaining_accounts_passes_them_through() {
        let extra = pk(200);
        let ix = deposit_ix(
            &deposit_accounts(),
            1,
            None,
            &[AccountMeta::new_readonly(extra, false)],
        );
        assert_eq!(ix.accounts.len(), 8);
        assert_eq!(ix.accounts[7].pubkey, extra);
    }

    #[test]
    fn initialize_marginfi_account_has_two_signers() {
        let ix = initialize_marginfi_account_ix(&InitializeMarginfiAccounts {
            marginfi_group: pk(1),
            marginfi_account: pk(2),
            authority: pk(3),
            fee_payer: pk(4),
            system_program: pk(5),
        });
        assert_eq!(&ix.data[..8], &IX_MARGINFI_ACCOUNT_INITIALIZE);
        assert_eq!(ix.data.len(), 8);

        let signer_count = ix.accounts.iter().filter(|a| a.is_signer).count();
        assert_eq!(signer_count, 3);
    }
}
