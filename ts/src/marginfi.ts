/**
 * marginfi v0.1.8 byte-layout reader for Bank + MarginfiAccount + Balance.
 *
 * ⚠️ yDelta targets marginfi **v0.1.8** (program `MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA`).
 * The `@mrgnlabs/marginfi-client-v2` npm package tracks v2.x and does NOT
 * match v0.1.8's account layouts — never use it for state reads. Authoritative
 * byte offsets live in `programs/marginfi-mocks/src/state.rs` (the v0.1.8 mock
 * the program targets) and `programs/ydelta/src/validation/marginfi_checkers.rs`
 * (the on-chain decoders). Mirror those, not the npm client.
 */
import { PublicKey } from '@solana/web3.js';

import { FP48_SHIFT } from './constants.js';
import { readPubkey, readU16, readU32, readU64, readU8, view } from './accounts/_read.js';

export const ANCHOR_DISC_LEN = 8;
export const BANK_BODY_SIZE = 1856;
export const BANK_ACCOUNT_SIZE = ANCHOR_DISC_LEN + BANK_BODY_SIZE;
export const MARGINFI_ACCOUNT_BODY_SIZE = 2304;
export const MARGINFI_ACCOUNT_TOTAL_SIZE = ANCHOR_DISC_LEN + MARGINFI_ACCOUNT_BODY_SIZE;
export const BALANCE_SIZE = 104;

export const BANK_DISCRIMINATOR = Uint8Array.from([142, 49, 166, 242, 50, 66, 97, 188]);
export const MARGINFI_ACCOUNT_DISCRIMINATOR = Uint8Array.from([67, 178, 130, 109, 126, 114, 28, 42]);

export enum OracleSetup {
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

// I80F48 → fp48. Negative bit-patterns clamp to 0 (matches on-chain
// wrapped_i80f48_to_u128). Bank share-values and weights are never < 0.
export function readWrappedI80F48ToFp48(dv: DataView, off: number): bigint {
  const lo = dv.getBigUint64(off, true);
  const hi = dv.getBigInt64(off + 8, true);
  if (hi < 0n) return 0n;
  return (hi << 64n) | lo;
}

export interface Bank {
  mint: PublicKey;
  mintDecimals: number;
  group: PublicKey;
  assetShareValueFp48: bigint;
  liabilityShareValueFp48: bigint;
  liquidityVault: PublicKey;
  assetWeightInitFp48: bigint;
  assetWeightMaintFp48: bigint;
  liabilityWeightInitFp48: bigint;
  liabilityWeightMaintFp48: bigint;
  oracleSetup: OracleSetup;
  oracleKeys: PublicKey[];
  oracleMaxAgeSeconds: number;
  /** Confidence cap × u32::MAX; divide by 2^32 − 1 for a 0..1 fraction. */
  oracleMaxConfidenceRawU32: number;
  /**
   * Current interest rates from marginfi's on-chain `BankCache`, in bps.
   * `cachedLendingRateBps` is the supplier (deposit) APR; `borrowing` is what
   * borrowers pay; `base` is the pre-fee curve output. marginfi refreshes
   * these on every accrue.
   */
  cachedBaseRateBps: number;
  cachedLendingRateBps: number;
  cachedBorrowingRateBps: number;
}

// Bank body offsets (post 8-byte anchor disc):
//   @0    mint                        Pubkey
//   @32   mint_decimals               u8
//   @33   group                       Pubkey
//   @72   asset_share_value           WrappedI80F48
//   @88   liability_share_value       WrappedI80F48
//   @104  liquidity_vault             Pubkey
//   @288  BankConfig start:
//     +0  asset_weight_init           WrappedI80F48
//     +16 asset_weight_maint          WrappedI80F48
//     +32 liability_weight_init       WrappedI80F48
//     +48 liability_weight_maint      WrappedI80F48
//     +313 oracle_setup               u8
//     +314 oracle_keys[5]             5 × Pubkey
//     +504 oracle_max_age             u16
//     +508 oracle_max_confidence      u32
//   @1368 BankCache (current rates, u32 each, milli-scaled):
//     +0  base_rate                   u32   (body 1368)
//     +4  lending_rate (supply APR)   u32   (body 1372)
//     +8  borrowing_rate              u32   (body 1376)
const BANK_CONFIG_BODY_OFFSET = 288;
// BankCache rate offsets within the bank body. Located against the live
// mainnet USDC + SOL banks (lending < base < borrowing, both plausible) — the
// v2 layout the in-repo IDL fixture describes does NOT match the deployed
// program here, so these are pinned empirically rather than from that fixture.
const BANK_CACHE_BASE_RATE = 1368;
const BANK_CACHE_LENDING_RATE = 1372;
const BANK_CACHE_BORROWING_RATE = 1376;

/**
 * marginfi caches rate APRs as a u32 via `milli_to_u32`: `u32 = apr/10 ×
 * u32::MAX` (so apr 1.0 → u32::MAX/10). Inverse, returned in bps:
 * `bps = u32 × 100_000 / u32::MAX`.
 */
function milliRateToBps(raw: number): number {
  return (raw * 100_000) / 0xffff_ffff;
}

function checkDiscriminator(data: Uint8Array | Buffer, expected: Uint8Array, label: string): void {
  for (let i = 0; i < ANCHOR_DISC_LEN; i++) {
    if (data[i] !== expected[i]) {
      throw new Error(`${label}: bad anchor discriminator`);
    }
  }
}

export function decodeBank(data: Uint8Array | Buffer): Bank {
  if (data.byteLength < BANK_ACCOUNT_SIZE) {
    throw new RangeError(`decodeBank: account too small (${data.byteLength} < ${BANK_ACCOUNT_SIZE})`);
  }
  checkDiscriminator(data, BANK_DISCRIMINATOR, 'decodeBank');
  const body = new Uint8Array(data.buffer, data.byteOffset + ANCHOR_DISC_LEN, BANK_BODY_SIZE);
  const dv = view(body);

  const oracleKeys: PublicKey[] = [];
  for (let i = 0; i < 5; i++) {
    oracleKeys.push(readPubkey(dv, BANK_CONFIG_BODY_OFFSET + 314 + i * 32));
  }

  return {
    mint: readPubkey(dv, 0),
    mintDecimals: readU8(dv, 32),
    group: readPubkey(dv, 33),
    assetShareValueFp48: readWrappedI80F48ToFp48(dv, 72),
    liabilityShareValueFp48: readWrappedI80F48ToFp48(dv, 88),
    liquidityVault: readPubkey(dv, 104),
    assetWeightInitFp48: readWrappedI80F48ToFp48(dv, BANK_CONFIG_BODY_OFFSET + 0),
    assetWeightMaintFp48: readWrappedI80F48ToFp48(dv, BANK_CONFIG_BODY_OFFSET + 16),
    liabilityWeightInitFp48: readWrappedI80F48ToFp48(dv, BANK_CONFIG_BODY_OFFSET + 32),
    liabilityWeightMaintFp48: readWrappedI80F48ToFp48(dv, BANK_CONFIG_BODY_OFFSET + 48),
    oracleSetup: readU8(dv, BANK_CONFIG_BODY_OFFSET + 313) as OracleSetup,
    oracleKeys,
    oracleMaxAgeSeconds: readU16(dv, BANK_CONFIG_BODY_OFFSET + 504),
    oracleMaxConfidenceRawU32: readU32(dv, BANK_CONFIG_BODY_OFFSET + 508),
    cachedBaseRateBps: milliRateToBps(readU32(dv, BANK_CACHE_BASE_RATE)),
    cachedLendingRateBps: milliRateToBps(readU32(dv, BANK_CACHE_LENDING_RATE)),
    cachedBorrowingRateBps: milliRateToBps(readU32(dv, BANK_CACHE_BORROWING_RATE)),
  };
}

/**
 * Convert a nominal annual rate (APR, bps) to a compounded APY (bps), matching
 * marginfi's UI convention of hourly compounding (`(1 + apr/N)^N − 1`, N =
 * 24×365). marginfi caches rates as APRs but displays the compounded APY.
 */
export function aprToApyBps(aprBps: number): number {
  if (aprBps <= 0) return 0;
  const N = 24 * 365;
  const apr = aprBps / 10_000;
  return ((1 + apr / N) ** N - 1) * 10_000;
}

/**
 * marginfi supply (deposit) APY for this bank, in bps — the rate idle deposits
 * earn from marginfi, matching marginfi's displayed APY. Read from the bank's
 * cached `lending_rate_apr` (refreshed on every accrue; already folds in
 * utilization + the fee split) and compounded to APY.
 */
export function marginfiSupplyApyBps(bank: Bank): number {
  return aprToApyBps(Math.max(0, bank.cachedLendingRateBps));
}

export function bankPrimaryOracle(bank: Bank): PublicKey {
  return bank.oracleKeys[0];
}

export function oracleMaxConfidenceFraction(bank: Bank): number {
  return bank.oracleMaxConfidenceRawU32 / 0xffff_ffff;
}

export interface Balance {
  active: boolean;
  bankPk: PublicKey;
  bankAssetTag: number;
  tag: number;
  assetSharesFp48: bigint;
  liabilitySharesFp48: bigint;
  emissionsOutstandingFp48: bigint;
  lastUpdate: bigint;
}

export interface MarginfiAccount {
  group: PublicKey;
  authority: PublicKey;
  balances: Balance[];
}

// Balance offsets within MarginfiAccount.balances[i]:
//   @0   active                  u8
//   @1   bank_pk                 Pubkey
//   @33  bank_asset_tag          u8
//   @34  tag                     u16
//   @40  asset_shares            WrappedI80F48
//   @56  liability_shares        WrappedI80F48
//   @72  emissions_outstanding   WrappedI80F48
//   @88  last_update             u64
function decodeBalance(dv: DataView, off: number): Balance {
  return {
    active: readU8(dv, off + 0) !== 0,
    bankPk: readPubkey(dv, off + 1),
    bankAssetTag: readU8(dv, off + 33),
    tag: readU16(dv, off + 34),
    assetSharesFp48: readWrappedI80F48ToFp48(dv, off + 40),
    liabilitySharesFp48: readWrappedI80F48ToFp48(dv, off + 56),
    emissionsOutstandingFp48: readWrappedI80F48ToFp48(dv, off + 72),
    lastUpdate: readU64(dv, off + 88),
  };
}

export function decodeMarginfiAccount(data: Uint8Array | Buffer): MarginfiAccount {
  if (data.byteLength < MARGINFI_ACCOUNT_TOTAL_SIZE) {
    throw new RangeError(`decodeMarginfiAccount: account too small (${data.byteLength})`);
  }
  checkDiscriminator(data, MARGINFI_ACCOUNT_DISCRIMINATOR, 'decodeMarginfiAccount');
  const body = new Uint8Array(
    data.buffer,
    data.byteOffset + ANCHOR_DISC_LEN,
    MARGINFI_ACCOUNT_BODY_SIZE,
  );
  const dv = view(body);
  const balances: Balance[] = [];
  for (let i = 0; i < 16; i++) {
    balances.push(decodeBalance(dv, 64 + i * BALANCE_SIZE));
  }
  return {
    group: readPubkey(dv, 0),
    authority: readPubkey(dv, 32),
    balances,
  };
}

export function findActiveBalance(account: MarginfiAccount, bank: PublicKey): Balance | undefined {
  return account.balances.find((b) => b.active && b.bankPk.equals(bank));
}

export function bankSharesToAtoms(shares: bigint, shareValueFp48: bigint): bigint {
  return (shares * shareValueFp48) >> FP48_SHIFT;
}

export function bankAtomsToShares(atoms: bigint, shareValueFp48: bigint): bigint {
  if (shareValueFp48 === 0n) {
    throw new RangeError('bankAtomsToShares: zero share value');
  }
  return (atoms << FP48_SHIFT) / shareValueFp48;
}
