use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::discriminator::{
    BANK_DISCRIMINATOR, MARGINFI_ACCOUNT_DISCRIMINATOR, MARGINFI_GROUP_DISCRIMINATOR,
};
use crate::wire::WrappedI80F48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MarginfiMocksError {
    InvalidDiscriminator = 200,
    AccountTooSmall = 201,
}

impl From<MarginfiMocksError> for ProgramError {
    fn from(e: MarginfiMocksError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

const ANCHOR_HEADER_LEN: usize = 8;

pub const BANK_BODY_SIZE: usize = 1856;

pub const BANK_ACCOUNT_SIZE: usize = ANCHOR_HEADER_LEN + BANK_BODY_SIZE;

#[allow(dead_code)]
const BANK_HOT_FIELDS_SIZE: usize = 138;
#[allow(dead_code)]
const BANK_OPAQUE_TAIL_SIZE: usize = BANK_BODY_SIZE - BANK_HOT_FIELDS_SIZE;

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct Bank {
    pub mint: Pubkey,
    pub mint_decimals: u8,
    pub group: Pubkey,
    pub _pad0: [u8; 7],
    pub asset_share_value: WrappedI80F48,
    pub liability_share_value: WrappedI80F48,
    pub liquidity_vault: Pubkey,
    pub liquidity_vault_bump: u8,
    pub liquidity_vault_authority_bump: u8,

    _tail0: [u8; 1024],
    _tail1: [u8; 512],
    _tail2: [u8; 128],
    _tail3: [u8; 32],
    _tail4: [u8; 16],
    _tail5: [u8; 6],
}
const_assert_eq!(size_of::<Bank>(), BANK_BODY_SIZE);

const_assert_eq!(BANK_HOT_FIELDS_SIZE + BANK_OPAQUE_TAIL_SIZE, BANK_BODY_SIZE);

impl Default for Bank {
    fn default() -> Self {
        Self {
            mint: Pubkey::default(),
            mint_decimals: 0,
            group: Pubkey::default(),
            _pad0: [0; 7],
            asset_share_value: WrappedI80F48::ZERO,
            liability_share_value: WrappedI80F48::ZERO,
            liquidity_vault: Pubkey::default(),
            liquidity_vault_bump: 0,
            liquidity_vault_authority_bump: 0,
            _tail0: [0; 1024],
            _tail1: [0; 512],
            _tail2: [0; 128],
            _tail3: [0; 32],
            _tail4: [0; 16],
            _tail5: [0; 6],
        }
    }
}

impl Bank {
    pub fn try_from_account_data(data: &[u8]) -> Result<&Self, MarginfiMocksError> {
        // M-31: require exact size. Accepting `>=` would silently absorb
        // a future marginfi `Bank` layout growth — any new trailing
        // fields would shift our byte-offset readers (M-29).
        if data.len() != BANK_ACCOUNT_SIZE {
            return Err(MarginfiMocksError::AccountTooSmall);
        }
        if &data[..ANCHOR_HEADER_LEN] != &BANK_DISCRIMINATOR {
            return Err(MarginfiMocksError::InvalidDiscriminator);
        }
        let body = &data[ANCHOR_HEADER_LEN..BANK_ACCOUNT_SIZE];
        Ok(bytemuck::from_bytes::<Self>(body))
    }
}

const BANK_CONFIG_BODY_OFFSET: usize = 288;

pub const BANK_CONFIG_SIZE: usize = 544;

const BC_ASSET_WEIGHT_INIT: usize = 0;
const BC_ASSET_WEIGHT_MAINT: usize = 16;
const BC_LIABILITY_WEIGHT_INIT: usize = 32;
const BC_LIABILITY_WEIGHT_MAINT: usize = 48;
const BC_ORACLE_SETUP: usize = 313;
const BC_ORACLE_KEYS: usize = 314;
const BC_ORACLE_MAX_AGE: usize = 504;
const BC_ORACLE_MAX_CONFIDENCE: usize = 508;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OracleSetup {
    None = 0,
    PythLegacy = 1,
    SwitchboardV2 = 2,
    PythPushOracle = 3,
    SwitchboardPull = 4,
    StakedWithPythPush = 5,
    KaminoPythPush = 6,
    KaminoSwitchboardPull = 7,
    Fixed = 8,
    DriftPythPull = 9,
    DriftSwitchboardPull = 10,
    SolendPythPull = 11,
    SolendSwitchboardPull = 12,
    FixedKamino = 13,
    FixedDrift = 14,
    JuplendPythPull = 15,
    JuplendSwitchboardPull = 16,
    FixedJuplend = 17,
}

impl OracleSetup {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::PythLegacy),
            2 => Some(Self::SwitchboardV2),
            3 => Some(Self::PythPushOracle),
            4 => Some(Self::SwitchboardPull),
            5 => Some(Self::StakedWithPythPush),
            6 => Some(Self::KaminoPythPush),
            7 => Some(Self::KaminoSwitchboardPull),
            8 => Some(Self::Fixed),
            9 => Some(Self::DriftPythPull),
            10 => Some(Self::DriftSwitchboardPull),
            11 => Some(Self::SolendPythPull),
            12 => Some(Self::SolendSwitchboardPull),
            13 => Some(Self::FixedKamino),
            14 => Some(Self::FixedDrift),
            15 => Some(Self::JuplendPythPull),
            16 => Some(Self::JuplendSwitchboardPull),
            17 => Some(Self::FixedJuplend),
            _ => None,
        }
    }

    pub fn is_fixed_price(self) -> bool {
        matches!(
            self,
            Self::Fixed | Self::FixedKamino | Self::FixedDrift | Self::FixedJuplend,
        )
    }

    pub fn is_single_oracle(self) -> bool {
        matches!(
            self,
            Self::PythPushOracle
                | Self::SwitchboardPull
                | Self::KaminoPythPush
                | Self::KaminoSwitchboardPull
                | Self::DriftPythPull
                | Self::DriftSwitchboardPull
                | Self::SolendPythPull
                | Self::SolendSwitchboardPull
                | Self::JuplendPythPull
                | Self::JuplendSwitchboardPull,
        )
    }
}

#[derive(Clone, Copy)]
pub struct BankConfigView<'a> {
    raw: &'a [u8; BANK_CONFIG_SIZE],
}

impl<'a> BankConfigView<'a> {
    pub fn try_from_account_data(data: &'a [u8]) -> Result<Self, MarginfiMocksError> {
        // M-31: exact size — see Bank::try_from_account_data above.
        if data.len() != BANK_ACCOUNT_SIZE {
            return Err(MarginfiMocksError::AccountTooSmall);
        }
        if &data[..ANCHOR_HEADER_LEN] != &BANK_DISCRIMINATOR {
            return Err(MarginfiMocksError::InvalidDiscriminator);
        }
        let body_start = ANCHOR_HEADER_LEN;
        let cfg_start = body_start + BANK_CONFIG_BODY_OFFSET;
        let cfg_end = cfg_start + BANK_CONFIG_SIZE;
        let raw: &[u8; BANK_CONFIG_SIZE] = data[cfg_start..cfg_end].try_into().unwrap();
        Ok(Self { raw })
    }

    fn read_wrapped(&self, off: usize) -> WrappedI80F48 {
        let bytes: [u8; 16] = self.raw[off..off + 16].try_into().unwrap();
        WrappedI80F48 { value: bytes }
    }

    pub fn asset_weight_init(&self) -> WrappedI80F48 {
        self.read_wrapped(BC_ASSET_WEIGHT_INIT)
    }
    pub fn asset_weight_maint(&self) -> WrappedI80F48 {
        self.read_wrapped(BC_ASSET_WEIGHT_MAINT)
    }
    pub fn liability_weight_init(&self) -> WrappedI80F48 {
        self.read_wrapped(BC_LIABILITY_WEIGHT_INIT)
    }
    pub fn liability_weight_maint(&self) -> WrappedI80F48 {
        self.read_wrapped(BC_LIABILITY_WEIGHT_MAINT)
    }

    pub fn oracle_setup_raw(&self) -> u8 {
        self.raw[BC_ORACLE_SETUP]
    }

    pub fn oracle_setup(&self) -> Option<OracleSetup> {
        OracleSetup::from_u8(self.oracle_setup_raw())
    }

    pub fn expected_oracle_account_count(&self) -> u8 {
        match self.oracle_setup() {
            Some(setup) if setup == OracleSetup::None || setup.is_fixed_price() => 0,
            Some(OracleSetup::StakedWithPythPush) => 3,
            _ => 1,
        }
    }

    pub fn primary_oracle(&self) -> Pubkey {
        self.oracle_key(0)
    }

    pub fn oracle_key(&self, idx: usize) -> Pubkey {
        assert!(idx < 5, "oracle_keys index out of range");
        let off = BC_ORACLE_KEYS + idx * 32;
        let bytes: [u8; 32] = self.raw[off..off + 32].try_into().unwrap();
        Pubkey::new_from_array(bytes)
    }

    pub fn oracle_max_age(&self) -> u16 {
        u16::from_le_bytes(
            self.raw[BC_ORACLE_MAX_AGE..BC_ORACLE_MAX_AGE + 2]
                .try_into()
                .unwrap(),
        )
    }

    pub fn oracle_max_confidence(&self) -> u32 {
        u32::from_le_bytes(
            self.raw[BC_ORACLE_MAX_CONFIDENCE..BC_ORACLE_MAX_CONFIDENCE + 4]
                .try_into()
                .unwrap(),
        )
    }
}

pub const BALANCE_SIZE: usize = 104;

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Pod, Zeroable)]
pub struct Balance {
    pub active: u8,
    pub bank_pk: Pubkey,
    pub bank_asset_tag: u8,
    pub tag: u16,
    pub _pad0: [u8; 4],
    pub asset_shares: WrappedI80F48,
    pub liability_shares: WrappedI80F48,
    pub emissions_outstanding: WrappedI80F48,
    pub last_update: u64,
    pub _padding: [u64; 1],
}
const_assert_eq!(size_of::<Balance>(), BALANCE_SIZE);

impl Balance {
    pub fn is_active(&self) -> bool {
        self.active != 0
    }
}

pub const MARGINFI_ACCOUNT_BODY_SIZE: usize = 2304;
pub const MARGINFI_ACCOUNT_TOTAL_SIZE: usize = ANCHOR_HEADER_LEN + MARGINFI_ACCOUNT_BODY_SIZE;

#[allow(dead_code)]
const MARGINFI_ACCOUNT_HOT_FIELDS_SIZE: usize = 64 + BALANCE_SIZE * 16;
#[allow(dead_code)]
const MARGINFI_ACCOUNT_OPAQUE_TAIL_SIZE: usize =
    MARGINFI_ACCOUNT_BODY_SIZE - MARGINFI_ACCOUNT_HOT_FIELDS_SIZE;

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct MarginfiAccount {
    pub group: Pubkey,
    pub authority: Pubkey,
    pub balances: [Balance; 16],

    _tail0: [u8; 512],
    _tail1: [u8; 64],
}
const_assert_eq!(size_of::<MarginfiAccount>(), MARGINFI_ACCOUNT_BODY_SIZE);
const_assert_eq!(
    MARGINFI_ACCOUNT_HOT_FIELDS_SIZE + MARGINFI_ACCOUNT_OPAQUE_TAIL_SIZE,
    MARGINFI_ACCOUNT_BODY_SIZE
);

impl Default for MarginfiAccount {
    fn default() -> Self {
        Self {
            group: Pubkey::default(),
            authority: Pubkey::default(),
            balances: [Balance::default(); 16],
            _tail0: [0; 512],
            _tail1: [0; 64],
        }
    }
}

impl MarginfiAccount {
    pub fn try_from_account_data(data: &[u8]) -> Result<&Self, MarginfiMocksError> {
        if data.len() < MARGINFI_ACCOUNT_TOTAL_SIZE {
            return Err(MarginfiMocksError::AccountTooSmall);
        }
        if &data[..ANCHOR_HEADER_LEN] != &MARGINFI_ACCOUNT_DISCRIMINATOR {
            return Err(MarginfiMocksError::InvalidDiscriminator);
        }
        let body = &data[ANCHOR_HEADER_LEN..MARGINFI_ACCOUNT_TOTAL_SIZE];
        Ok(bytemuck::from_bytes::<Self>(body))
    }

    pub fn find_balance(&self, bank: &Pubkey) -> Option<&Balance> {
        self.balances
            .iter()
            .find(|b| b.is_active() && &b.bank_pk == bank)
    }
}

pub fn assert_is_marginfi_account(data: &[u8]) -> Result<(), MarginfiMocksError> {
    if data.len() < ANCHOR_HEADER_LEN {
        return Err(MarginfiMocksError::AccountTooSmall);
    }
    if &data[..ANCHOR_HEADER_LEN] != &MARGINFI_ACCOUNT_DISCRIMINATOR {
        return Err(MarginfiMocksError::InvalidDiscriminator);
    }
    Ok(())
}

pub fn assert_is_marginfi_group(data: &[u8]) -> Result<(), MarginfiMocksError> {
    if data.len() < ANCHOR_HEADER_LEN {
        return Err(MarginfiMocksError::AccountTooSmall);
    }
    if &data[..ANCHOR_HEADER_LEN] != &MARGINFI_GROUP_DISCRIMINATOR {
        return Err(MarginfiMocksError::InvalidDiscriminator);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_bank() -> Vec<u8> {
        let bank = Bank {
            mint: Pubkey::new_from_array([0xAA; 32]),
            mint_decimals: 9,
            group: Pubkey::new_from_array([0xBB; 32]),
            asset_share_value: WrappedI80F48::from_i128_bits(1i128 << 48),
            liability_share_value: WrappedI80F48::from_i128_bits(2i128 << 48),
            liquidity_vault: Pubkey::new_from_array([0xCC; 32]),
            liquidity_vault_bump: 254,
            liquidity_vault_authority_bump: 253,
            ..Default::default()
        };
        let mut buf = Vec::with_capacity(BANK_ACCOUNT_SIZE);
        buf.extend_from_slice(&BANK_DISCRIMINATOR);
        buf.extend_from_slice(bytemuck::bytes_of(&bank));
        assert_eq!(buf.len(), BANK_ACCOUNT_SIZE);
        buf
    }

    #[test]
    fn bank_size_matches_v018_idl() {
        assert_eq!(size_of::<Bank>(), 1856);
        assert_eq!(BANK_BODY_SIZE, 1856);
        assert_eq!(BANK_ACCOUNT_SIZE, 1864);
    }

    #[test]
    fn bank_zero_copy_reads_synthesised_account() {
        let data = synth_bank();
        let b = Bank::try_from_account_data(&data).unwrap();
        assert_eq!(b.mint_decimals, 9);
        assert_eq!(b.liquidity_vault_bump, 254);
        assert_eq!(b.liquidity_vault_authority_bump, 253);
        assert_eq!(b.asset_share_value.to_i128_bits(), 1i128 << 48);
        assert_eq!(b.liability_share_value.to_i128_bits(), 2i128 << 48);
        assert_eq!(b.mint, Pubkey::new_from_array([0xAA; 32]));
        assert_eq!(b.group, Pubkey::new_from_array([0xBB; 32]));
        assert_eq!(b.liquidity_vault, Pubkey::new_from_array([0xCC; 32]));
    }

    #[test]
    fn bank_rejects_bad_discriminator() {
        let mut data = synth_bank();
        data[0] = 0;
        assert_eq!(
            Bank::try_from_account_data(&data).unwrap_err(),
            MarginfiMocksError::InvalidDiscriminator
        );
    }

    #[test]
    fn bank_rejects_truncated_data() {
        let data = vec![0u8; 4];
        assert_eq!(
            Bank::try_from_account_data(&data).unwrap_err(),
            MarginfiMocksError::AccountTooSmall
        );
        let mut not_quite = vec![0u8; BANK_ACCOUNT_SIZE - 1];
        not_quite[..8].copy_from_slice(&BANK_DISCRIMINATOR);
        assert_eq!(
            Bank::try_from_account_data(&not_quite).unwrap_err(),
            MarginfiMocksError::AccountTooSmall
        );
    }

    #[test]
    fn bank_config_view_reads_at_documented_offsets() {
        let mut data = synth_bank();
        let cfg_start = ANCHOR_HEADER_LEN + BANK_CONFIG_BODY_OFFSET;

        let awi = 1i128 << 47;
        data[cfg_start + BC_ASSET_WEIGHT_INIT..cfg_start + BC_ASSET_WEIGHT_INIT + 16]
            .copy_from_slice(&awi.to_le_bytes());

        let lwi: i128 = (12i128 << 48) / 10;
        data[cfg_start + BC_LIABILITY_WEIGHT_INIT..cfg_start + BC_LIABILITY_WEIGHT_INIT + 16]
            .copy_from_slice(&lwi.to_le_bytes());

        data[cfg_start + BC_ORACLE_SETUP] = 4;

        let oracle = Pubkey::new_from_array([0xEE; 32]);
        data[cfg_start + BC_ORACLE_KEYS..cfg_start + BC_ORACLE_KEYS + 32]
            .copy_from_slice(oracle.as_ref());

        data[cfg_start + BC_ORACLE_MAX_AGE..cfg_start + BC_ORACLE_MAX_AGE + 2]
            .copy_from_slice(&70u16.to_le_bytes());

        let conf: u32 = u32::MAX / 20;
        data[cfg_start + BC_ORACLE_MAX_CONFIDENCE..cfg_start + BC_ORACLE_MAX_CONFIDENCE + 4]
            .copy_from_slice(&conf.to_le_bytes());

        let view = BankConfigView::try_from_account_data(&data).unwrap();
        assert_eq!(view.asset_weight_init().to_i128_bits(), awi);
        assert_eq!(view.liability_weight_init().to_i128_bits(), lwi);
        assert_eq!(view.oracle_setup_raw(), 4);
        assert_eq!(view.oracle_setup(), Some(OracleSetup::SwitchboardPull));
        assert_eq!(view.primary_oracle(), oracle);
        assert_eq!(view.oracle_max_age(), 70);
        assert_eq!(view.oracle_max_confidence(), conf);
    }

    #[test]
    fn bank_config_view_rejects_truncated() {
        let small = vec![0u8; 16];
        assert!(BankConfigView::try_from_account_data(&small).is_err());
    }

    #[test]
    fn marginfi_account_discriminator_validator() {
        let mut data = vec![0u8; 64];
        data[..8].copy_from_slice(&MARGINFI_ACCOUNT_DISCRIMINATOR);
        assert!(assert_is_marginfi_account(&data).is_ok());
    }

    #[test]
    fn marginfi_group_discriminator_validator() {
        let mut data = vec![0u8; 64];
        data[..8].copy_from_slice(&MARGINFI_GROUP_DISCRIMINATOR);
        assert!(assert_is_marginfi_group(&data).is_ok());
    }

    fn synth_marginfi_account_with_balance(bank_pk: Pubkey, asset_shares: i128) -> Vec<u8> {
        let mut acc = MarginfiAccount::default();
        acc.group = Pubkey::new_from_array([0x11; 32]);
        acc.authority = Pubkey::new_from_array([0x22; 32]);
        acc.balances[0] = Balance {
            active: 1,
            bank_pk,
            asset_shares: WrappedI80F48::from_i128_bits(asset_shares),
            ..Default::default()
        };
        let mut buf = Vec::with_capacity(MARGINFI_ACCOUNT_TOTAL_SIZE);
        buf.extend_from_slice(&MARGINFI_ACCOUNT_DISCRIMINATOR);
        buf.extend_from_slice(bytemuck::bytes_of(&acc));
        assert_eq!(buf.len(), MARGINFI_ACCOUNT_TOTAL_SIZE);
        buf
    }

    #[test]
    fn marginfi_account_size_matches_v018_idl() {
        assert_eq!(size_of::<MarginfiAccount>(), 2304);
        assert_eq!(MARGINFI_ACCOUNT_BODY_SIZE, 2304);
        assert_eq!(MARGINFI_ACCOUNT_TOTAL_SIZE, 2312);
        assert_eq!(size_of::<Balance>(), 104);
    }

    #[test]
    fn marginfi_account_zero_copy_reads_synthesised_balance() {
        let bank = Pubkey::new_from_array([0x33; 32]);
        let data = synth_marginfi_account_with_balance(bank, 5_000i128 << 48);
        let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
        let bal = mfi.find_balance(&bank).expect("balance present");
        assert!(bal.is_active());
        assert_eq!(bal.bank_pk, bank);
        assert_eq!(bal.asset_shares.to_i128_bits(), 5_000i128 << 48);
    }

    #[test]
    fn marginfi_account_find_balance_returns_none_for_unknown_bank() {
        let bank = Pubkey::new_from_array([0x33; 32]);
        let data = synth_marginfi_account_with_balance(bank, 0);
        let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
        let other = Pubkey::new_from_array([0x44; 32]);
        assert!(mfi.find_balance(&other).is_none());
    }

    #[test]
    fn marginfi_account_find_balance_skips_inactive_entries() {
        let bank = Pubkey::new_from_array([0x33; 32]);
        let mut acc = MarginfiAccount::default();

        acc.balances[0] = Balance {
            active: 0,
            bank_pk: bank,
            asset_shares: WrappedI80F48::from_i128_bits(7 << 48),
            ..Default::default()
        };
        let mut buf = Vec::with_capacity(MARGINFI_ACCOUNT_TOTAL_SIZE);
        buf.extend_from_slice(&MARGINFI_ACCOUNT_DISCRIMINATOR);
        buf.extend_from_slice(bytemuck::bytes_of(&acc));
        let mfi = MarginfiAccount::try_from_account_data(&buf).unwrap();
        assert!(mfi.find_balance(&bank).is_none());
    }
}
