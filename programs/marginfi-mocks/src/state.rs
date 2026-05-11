//! Slim vendored views of marginfi v0.1.8 account headers. Each type is a
//! prefix of the actual on-chain struct — covering only the fields the
//! adapter reads — laid out as `#[repr(C)] + bytemuck::Pod` so we can
//! zero-copy-read it from account data via `bytemuck::from_bytes`.
//!
//! Why a prefix struct rather than the whole thing: marginfi's `Bank` is
//! ~2000 bytes with 50+ fields including nested `BankConfig`,
//! `EmodeSettings`, `BankCache`, `BankRateLimiter` etc. The adapter only
//! ever reads about 5 of those fields. Vendoring the whole struct would mean
//! tracking every upstream padding/layout change in nested types we don't
//! care about. The prefix struct fully describes the bytes we read; the
//! tail of the account is opaque and ignored.
//!
//! This mirrors marginfi's own pattern in their Kamino/Drift/Solend mocks:
//! they vendor `MinimalReserve` / `MinimalSpotMarket` / `SolendMinimalReserve`
//! with just the fields they need, opaquely padded to match the on-chain
//! layout's start (see `kamino-mocks/src/state.rs:14`).
//!
//! Source of truth: the v0.1.8 IDL at `tests/fixtures/marginfi.idl.json`.
//! Field order and explicit `_pad*` placement copied verbatim.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::discriminator::{
    BANK_DISCRIMINATOR, MARGINFI_ACCOUNT_DISCRIMINATOR, MARGINFI_GROUP_DISCRIMINATOR,
};
use crate::wire::WrappedI80F48;

/// Errors raised by the discriminator validators and zero-copy readers.
/// Sequential numbers in the 200..299 range to avoid collision with
/// `YdeltaError` (0..99) and `AdapterError` (100..199). Same convention
/// marginfi follows for its mock crates' error spaces.
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

/// Anchor lays out every account as: 8-byte discriminator + struct body.
const ANCHOR_HEADER_LEN: usize = 8;

/// Total byte size of marginfi's `Bank` body (post-discriminator).
/// Verified against the v0.1.8 IDL at `tests/fixtures/marginfi.idl.json`.
/// If marginfi mainnet upgrades to a Bank with a different size, the IDL
/// dump will report it and the static_assert below catches the drift.
pub const BANK_BODY_SIZE: usize = 1856;
/// Bank account total size including 8-byte Anchor discriminator.
pub const BANK_ACCOUNT_SIZE: usize = ANCHOR_HEADER_LEN + BANK_BODY_SIZE;

/// The portion of marginfi's `Bank` body the adapter reads, in declared
/// order. Plus an opaque tail that pads `Bank` out to the on-chain size.
/// Both constants only feed a `const_assert_eq!` invariant; rustc's
/// dead-code lint doesn't see macro uses.
#[allow(dead_code)]
const BANK_HOT_FIELDS_SIZE: usize = 138;
#[allow(dead_code)]
const BANK_OPAQUE_TAIL_SIZE: usize = BANK_BODY_SIZE - BANK_HOT_FIELDS_SIZE; // 1718

/// Slim view of marginfi's `Bank` account body. The first 138 bytes are
/// declared field-by-field (the ones the adapter actually reads); the
/// trailing 1718 bytes are an opaque `_tail` that brings the struct's size
/// up to the on-chain layout's exact 1856 bytes.
///
/// Why match the full size: `bytemuck::from_bytes::<Bank>(account.data)` —
/// the natural call site — requires the slice to be exactly
/// `size_of::<Bank>()`. Sizing the struct correctly means callers don't
/// have to slice-trim before reading, and a future regression where
/// upstream's Bank grows would surface as a static_assert mismatch the
/// next time the IDL is re-fetched.
///
/// `repr(C)` + every field at alignment 1 means there's no implicit
/// padding; the Pod derive verifies this at compile time. The tail is
/// split into bytemuck-supported chunk sizes (1024 + 512 + 128 + 32 + 16
/// + 6 = 1718) because bytemuck 1.7 without `min_const_generics` only
/// derives `Pod` for `[u8; N]` at specific N values.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct Bank {
    // ─── hot fields (138 bytes, read by the adapter) ───
    pub mint: Pubkey,
    pub mint_decimals: u8,
    pub group: Pubkey,
    pub _pad0: [u8; 7],
    pub asset_share_value: WrappedI80F48,
    pub liability_share_value: WrappedI80F48,
    pub liquidity_vault: Pubkey,
    pub liquidity_vault_bump: u8,
    pub liquidity_vault_authority_bump: u8,

    // ─── opaque tail (1718 bytes, ignored by the adapter) ───
    // Split into bytemuck-derive-friendly chunks. Total: 1024 + 512 + 128 +
    // 32 + 16 + 6 = 1718.
    _tail0: [u8; 1024],
    _tail1: [u8; 512],
    _tail2: [u8; 128],
    _tail3: [u8; 32],
    _tail4: [u8; 16],
    _tail5: [u8; 6],
}
const_assert_eq!(size_of::<Bank>(), BANK_BODY_SIZE);
// 32 + 1 + 32 + 7 + 16 + 16 + 32 + 1 + 1 = 138 hot fields
// 1024 + 512 + 128 + 32 + 16 + 6 = 1718 tail
// 138 + 1718 = 1856 ✓
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
    /// Borrow a `&Bank` view from raw account data. `data` must start with
    /// the 8-byte Anchor discriminator and contain the full `BANK_BODY_SIZE`
    /// bytes that follow. Returns errors on size or discriminator mismatch.
    pub fn try_from_account_data(data: &[u8]) -> Result<&Self, MarginfiMocksError> {
        if data.len() < BANK_ACCOUNT_SIZE {
            return Err(MarginfiMocksError::AccountTooSmall);
        }
        if &data[..ANCHOR_HEADER_LEN] != &BANK_DISCRIMINATOR {
            return Err(MarginfiMocksError::InvalidDiscriminator);
        }
        let body = &data[ANCHOR_HEADER_LEN..BANK_ACCOUNT_SIZE];
        Ok(bytemuck::from_bytes::<Self>(body))
    }
}

// ────────────────────── BankConfigView ──────────────────────
//
// Marginfi's `Bank.config: BankConfig` lives inside the opaque 1718-byte
// tail of our slim `Bank` view. Rather than re-laying out the tail to
// surface every config field, the view below reads at hardcoded byte
// offsets derived directly from the v0.1.8 IDL (`marginfi.idl.json`).
//
// **Offset derivation** — within the bank body (post-Anchor-disc):
// - `config` starts at offset 288 (`mint(32)+mint_decimals(1)+group(32)+
//   _pad0(7)+asset_share_value(16)+liability_share_value(16)+
//   liquidity_vault(32)+liquidity_vault_bump(1)+
//   liquidity_vault_authority_bump(1)+insurance_vault(32)+
//   insurance_vault_bump(1)+insurance_vault_authority_bump(1)+_pad1(4)+
//   collected_insurance_fees_outstanding(16)+fee_vault(32)+
//   fee_vault_bump(1)+fee_vault_authority_bump(1)+_pad2(6)+
//   collected_group_fees_outstanding(16)+total_liability_shares(16)+
//   total_asset_shares(16)+last_update(8) = 288`).
// - `BankConfig` itself is 544 bytes (matches numa's
//   `assert_struct_size!(BankConfig, 544)`).
// - Within `BankConfig`: weight fields at 0/16/32/48; oracle_setup at
//   313 (after `interest_rate_config(240)+operational_state(1)`);
//   oracle_keys[0] at 314; oracle_max_age at 504;
//   oracle_max_confidence at 508.

/// Byte offset within the Bank body where `BankConfig` begins.
const BANK_CONFIG_BODY_OFFSET: usize = 288;
/// Total size of `BankConfig` (verified against numa's
/// `assert_struct_size!(BankConfig, 544)`).
pub const BANK_CONFIG_SIZE: usize = 544;

const BC_ASSET_WEIGHT_INIT: usize = 0;
const BC_ASSET_WEIGHT_MAINT: usize = 16;
const BC_LIABILITY_WEIGHT_INIT: usize = 32;
const BC_LIABILITY_WEIGHT_MAINT: usize = 48;
const BC_ORACLE_SETUP: usize = 313;
const BC_ORACLE_KEYS: usize = 314;
const BC_ORACLE_MAX_AGE: usize = 504;
const BC_ORACLE_MAX_CONFIDENCE: usize = 508;

/// `OracleSetup` enum tags. Mirror of marginfi v2's tag set from
/// `type-crate/src/types/bank.rs::OracleSetup`. Variants 0..=5 carried
/// over from v0.1.8 unchanged; 6..=17 were added in v2 to cover the
/// Kamino / Drift / Solend / JupLend integrations and the Fixed-price
/// banks.
///
/// yDelta's order flow only supports single-oracle banks
/// (`PythPushOracle`, `SwitchboardPull`, plus `StakedWithPythPush` if a
/// fixture surfaces). The other v2 variants are recognised here so
/// `from_u8` round-trips cleanly off a real v2 bank's `oracle_setup`
/// byte, but `programs/ydelta/src/protocol/oracles.rs` will return
/// `OracleSetupUnsupported` for them — explicit reject beats silently
/// decoding the wrong account.
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

    /// True for variants whose price comes from the bank's own
    /// `fixed_price` field rather than an external oracle account.
    /// yDelta currently doesn't support these (no `fixed_price`
    /// decoder), but classification is useful so the adapter can
    /// return a precise "fixed banks not supported" error instead of a
    /// generic "wrong oracle count".
    pub fn is_fixed_price(self) -> bool {
        matches!(
            self,
            Self::Fixed | Self::FixedKamino | Self::FixedDrift | Self::FixedJuplend,
        )
    }

    /// True for variants that read from a single off-chain price feed
    /// (Pyth push or Switchboard pull). All v2 integration setups
    /// (Kamino/Drift/Solend/JupLend, both Pyth- and Switchboard-flavoured)
    /// fall under this category — they still use one oracle account at
    /// the CPI boundary even though the surrounding protocol differs.
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

/// Borrow-style view into a marginfi `Bank` account's `BankConfig`
/// region. All accessors read at hardcoded offsets — no copies, no
/// allocations, ProgramError-free at the borrow boundary (the caller
/// already validated the discriminator + size via
/// `Bank::try_from_account_data`).
#[derive(Clone, Copy)]
pub struct BankConfigView<'a> {
    raw: &'a [u8; BANK_CONFIG_SIZE],
}

impl<'a> BankConfigView<'a> {
    /// Borrow a `BankConfigView` from full account data (including the
    /// 8-byte Anchor discriminator). Validates discriminator + size
    /// before slicing.
    pub fn try_from_account_data(data: &'a [u8]) -> Result<Self, MarginfiMocksError> {
        if data.len() < BANK_ACCOUNT_SIZE {
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

    /// Number of oracle accounts marginfi v2 expects passed for this
    /// bank's setup at CPI time. The breakdown:
    /// - `None` / Fixed-price variants (`Fixed`, `FixedKamino`,
    ///   `FixedDrift`, `FixedJuplend`): 0 — price lives in the bank's
    ///   `fixed_price` field, no oracle account passed.
    /// - `StakedWithPythPush`: 3 (price feed + LST mint + LST stake pool).
    /// - All other recognised variants (Pyth Push, Switchboard Pull,
    ///   plus every v2 integration setup): 1.
    /// - Unknown / unparseable bytes: 1, treated as a single-oracle
    ///   bank by default. The yDelta-side `protocol/oracles.rs` then
    ///   rejects it with `OracleSetupUnsupported` because the variant
    ///   isn't in its allow-list.
    pub fn expected_oracle_account_count(&self) -> u8 {
        match self.oracle_setup() {
            Some(setup) if setup == OracleSetup::None || setup.is_fixed_price() => 0,
            Some(OracleSetup::StakedWithPythPush) => 3,
            _ => 1,
        }
    }

    /// Primary oracle pubkey (`oracle_keys[0]`). Most setups use only
    /// this slot; multi-key setups (Pyth-legacy + EMA, kamino combos)
    /// use higher slots, exposed via `oracle_key(idx)`.
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

// ────────────────────── Balance ──────────────────────

/// Total byte size of a marginfi `Balance` entry. `LendingAccount.balances`
/// is `[Balance; 16]`. Layout exact-match against v0.1.8 IDL.
pub const BALANCE_SIZE: usize = 104;

/// One slot in a `LendingAccount`'s `balances` array. Tracks a marginfi
/// account's stake and liability in a single bank.
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

// ────────────────────── MarginfiAccount ──────────────────────

pub const MARGINFI_ACCOUNT_BODY_SIZE: usize = 2304;
pub const MARGINFI_ACCOUNT_TOTAL_SIZE: usize = ANCHOR_HEADER_LEN + MARGINFI_ACCOUNT_BODY_SIZE;

// Both constants only feed a `const_assert_eq!` invariant; rustc's
// dead-code lint doesn't see macro uses.
#[allow(dead_code)]
const MARGINFI_ACCOUNT_HOT_FIELDS_SIZE: usize = 64 + BALANCE_SIZE * 16; // 1728
#[allow(dead_code)]
const MARGINFI_ACCOUNT_OPAQUE_TAIL_SIZE: usize =
    MARGINFI_ACCOUNT_BODY_SIZE - MARGINFI_ACCOUNT_HOT_FIELDS_SIZE; // 576

/// Slim view of a marginfi `MarginfiAccount` body. Exposes the fields
/// adapters read (`group`, `authority`, the 16-slot `balances` array) and
/// pads the trailing 576 bytes opaque to match the on-chain layout.
///
/// The `balances` array is the load-bearing piece: the adapter calls
/// `find_balance(bank_pk)` pre/post CPI to compute the share-balance delta
/// for the bank being touched.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct MarginfiAccount {
    pub group: Pubkey,
    pub authority: Pubkey,
    pub balances: [Balance; 16],

    // Opaque tail. Same chunk-split rationale as Bank: bytemuck 1.7
    // without `min_const_generics` only auto-derives Pod for specific
    // [u8; N] sizes. 512 + 64 = 576.
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
    /// Borrow a `&MarginfiAccount` view from raw account data (including
    /// the 8-byte Anchor discriminator).
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

    /// Return the active balance entry whose `bank_pk` matches `bank`,
    /// or `None` if the account has no position in that bank yet.
    pub fn find_balance(&self, bank: &Pubkey) -> Option<&Balance> {
        self.balances
            .iter()
            .find(|b| b.is_active() && &b.bank_pk == bank)
    }
}

// ────────────────────── Discriminator-only validators ──────────────────────

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

    /// Build a synthetic Bank account: 8-byte discriminator + full
    /// `BANK_BODY_SIZE`-byte body, with recognisable patterns in each field
    /// the adapter reads.
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
        // Sentinel: the v0.1.8 IDL says Bank's body is 1856 bytes. If
        // upstream upgrades, the IDL re-fetch will report a new size and
        // this test forces us to update both BANK_BODY_SIZE and the tail
        // chunk-split.
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
        // Even our slim view must reject an account smaller than the full
        // on-chain Bank size, so callers can pass `account.data` directly
        // without manual slicing.
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
        // Build a synthetic bank account, then poke recognisable bytes
        // directly at each documented BankConfig offset and verify the
        // view reads them back correctly. Catches off-by-one arithmetic
        // in the offset constants.
        let mut data = synth_bank();
        let cfg_start = ANCHOR_HEADER_LEN + BANK_CONFIG_BODY_OFFSET;

        // asset_weight_init = 0.5 fp48 (bit pattern 1<<47).
        let awi = 1i128 << 47;
        data[cfg_start + BC_ASSET_WEIGHT_INIT..cfg_start + BC_ASSET_WEIGHT_INIT + 16]
            .copy_from_slice(&awi.to_le_bytes());
        // liability_weight_init = 1.2 fp48 ≈ 1.2 × 2^48.
        let lwi: i128 = (12i128 << 48) / 10;
        data[cfg_start + BC_LIABILITY_WEIGHT_INIT..cfg_start + BC_LIABILITY_WEIGHT_INIT + 16]
            .copy_from_slice(&lwi.to_le_bytes());
        // oracle_setup = SwitchboardPull (4).
        data[cfg_start + BC_ORACLE_SETUP] = 4;
        // oracle_keys[0] = recognisable pubkey.
        let oracle = Pubkey::new_from_array([0xEE; 32]);
        data[cfg_start + BC_ORACLE_KEYS..cfg_start + BC_ORACLE_KEYS + 32]
            .copy_from_slice(oracle.as_ref());
        // oracle_max_age = 70 (typical mainnet config).
        data[cfg_start + BC_ORACLE_MAX_AGE..cfg_start + BC_ORACLE_MAX_AGE + 2]
            .copy_from_slice(&70u16.to_le_bytes());
        // oracle_max_confidence = 5% as u32 (≈ U32_MAX/20).
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

    // ────────────────────── MarginfiAccount tests ──────────────────────

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
        // Inactive entry pointing at the bank we're looking up — must be
        // skipped per `is_active`.
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
