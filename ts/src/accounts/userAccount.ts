/**
 * `UserAccount` — 128-byte header + variable dynamic region holding three
 * RB-trees (vault_positions, market_positions, open_loans) sharing one
 * 160-byte free list. Each tree-node payload is 144 bytes.
 *
 * Mirrors are downstream: the authoritative balances live on each
 * `ClaimedSeat` / `RiskProfileDepositorSeat`. The mirrors here let UIs
 * answer "show me everything this wallet has" without scanning every
 * market / vault.
 */
import { PublicKey } from '@solana/web3.js';

import {
  isNil,
  readI64,
  readPubkey,
  readU128,
  readU16,
  readU32,
  readU64,
  readU8,
  view,
} from './_read.js';
import { walkDescending } from './trees.js';

export const USER_ACCOUNT_FIXED_SIZE = 128;
export const USER_ACCOUNT_BLOCK_PAYLOAD_SIZE = 144;
export const USER_ACCOUNT_DISCRIMINANT = 0x79_64_65_6c_74_61_55_53n; // "ydeltaUS"

export interface UserAccountHeader {
  owner: PublicKey;
  vaultPositionsRootIndex: number;
  marketPositionsRootIndex: number;
  openLoansRootIndex: number;
  freeListHeadIndex: number;
  numBytesAllocated: number;
  vaultPositionCount: number;
  marketPositionCount: number;
  openLoanCount: number;
  bump: number;
  version: number;
}

export interface VaultPosition {
  vault: PublicKey;
  profileId: number;
  shares: bigint;
  snapshotSupplyYieldIndexScaled: bigint;
  snapshotDeltaYieldIndexScaled: bigint;
  lastUpdatedUnix: bigint;
}

export interface MarketPosition {
  market: PublicKey;
  seatIndexInMarket: number;
  debtWithdrawableShares: bigint;
  debtEncumberedShares: bigint;
  collateralWithdrawableShares: bigint;
  collateralEncumberedShares: bigint;
}

export interface UserLoanRef {
  loan: PublicKey;
  market: PublicKey;
  principalAtoms: bigint;
  startedAtUnix: bigint;
  maturesAtUnix: bigint;
  rateBps: number;
  /** 0 = Borrower, 1 = Lender. */
  role: number;
  /** 0 = UserWallet, 1 = GlobalVault. */
  counterpartyKind: number;
  /** Only meaningful when counterpartyKind == 1. */
  counterpartyProfileId: number;
}

export interface UserAccount {
  header: UserAccountHeader;
  vaultPositions: VaultPosition[];
  marketPositions: MarketPosition[];
  openLoans: UserLoanRef[];
}

/* ── Header ──────────────────────────────────────────────── */

export function decodeUserAccountHeader(data: Uint8Array | Buffer): UserAccountHeader {
  if (data.byteLength < USER_ACCOUNT_FIXED_SIZE) {
    throw new RangeError(`decodeUserAccountHeader: account too small`);
  }
  const dv = view(data);
  const disc = dv.getBigUint64(0, true);
  if (disc !== USER_ACCOUNT_DISCRIMINANT) {
    throw new Error(
      `decodeUserAccount: bad discriminator 0x${disc.toString(16)} (expected 0x${USER_ACCOUNT_DISCRIMINANT.toString(16)})`,
    );
  }
  return {
    owner: readPubkey(dv, 8),
    vaultPositionsRootIndex: readU32(dv, 40),
    marketPositionsRootIndex: readU32(dv, 44),
    openLoansRootIndex: readU32(dv, 48),
    freeListHeadIndex: readU32(dv, 52),
    numBytesAllocated: readU32(dv, 56),
    vaultPositionCount: readU16(dv, 60),
    marketPositionCount: readU16(dv, 62),
    openLoanCount: readU32(dv, 64),
    bump: readU8(dv, 68),
    version: readU8(dv, 69),
  };
}

/* ── Per-node decoders ───────────────────────────────────── */

export function decodeVaultPosition(payload: DataView): VaultPosition {
  return {
    vault: readPubkey(payload, 0),
    profileId: readU8(payload, 32),
    shares: readU128(payload, 48),
    snapshotSupplyYieldIndexScaled: readU128(payload, 64),
    snapshotDeltaYieldIndexScaled: readU128(payload, 80),
    lastUpdatedUnix: readI64(payload, 96),
  };
}

export function decodeMarketPosition(payload: DataView): MarketPosition {
  return {
    market: readPubkey(payload, 0),
    seatIndexInMarket: readU32(payload, 32),
    debtWithdrawableShares: readU128(payload, 48),
    debtEncumberedShares: readU128(payload, 64),
    collateralWithdrawableShares: readU128(payload, 80),
    collateralEncumberedShares: readU128(payload, 96),
  };
}

export function decodeUserLoanRef(payload: DataView): UserLoanRef {
  return {
    loan: readPubkey(payload, 0),
    market: readPubkey(payload, 32),
    principalAtoms: readU64(payload, 64),
    startedAtUnix: readI64(payload, 72),
    maturesAtUnix: readI64(payload, 80),
    rateBps: readU16(payload, 88),
    role: readU8(payload, 90),
    counterpartyKind: readU8(payload, 91),
    counterpartyProfileId: readU8(payload, 92),
  };
}

/* ── Full decode (header + walk every tree) ──────────────── */

export function decodeUserAccount(data: Uint8Array | Buffer): UserAccount {
  const header = decodeUserAccountHeader(data);
  const dynamicBytes = data.subarray
    ? data.subarray(USER_ACCOUNT_FIXED_SIZE)
    : new Uint8Array(data.buffer, data.byteOffset + USER_ACCOUNT_FIXED_SIZE);
  const dynamic = new DataView(
    dynamicBytes.buffer,
    dynamicBytes.byteOffset,
    dynamicBytes.byteLength,
  );

  const vaultPositions: VaultPosition[] = [];
  const marketPositions: MarketPosition[] = [];
  const openLoans: UserLoanRef[] = [];

  if (!isNil(header.vaultPositionsRootIndex)) {
    for (const { payload } of walkDescending(
      dynamic,
      header.vaultPositionsRootIndex,
      USER_ACCOUNT_BLOCK_PAYLOAD_SIZE,
    )) {
      vaultPositions.push(decodeVaultPosition(payload));
    }
  }
  if (!isNil(header.marketPositionsRootIndex)) {
    for (const { payload } of walkDescending(
      dynamic,
      header.marketPositionsRootIndex,
      USER_ACCOUNT_BLOCK_PAYLOAD_SIZE,
    )) {
      marketPositions.push(decodeMarketPosition(payload));
    }
  }
  if (!isNil(header.openLoansRootIndex)) {
    for (const { payload } of walkDescending(
      dynamic,
      header.openLoansRootIndex,
      USER_ACCOUNT_BLOCK_PAYLOAD_SIZE,
    )) {
      openLoans.push(decodeUserLoanRef(payload));
    }
  }

  return { header, vaultPositions, marketPositions, openLoans };
}
