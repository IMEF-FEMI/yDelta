/**
 * `GlobalVaultFixed` (320-byte header) + dynamic region holding three
 * RB-trees: `risk_profiles` (512-byte blocks), `claimed_seats`
 * (RiskProfileDepositorSeat — 160-byte blocks), `market_orders`
 * (RiskProfileOrderRef — 160-byte blocks). The latter two share a
 * 160-byte free list; profiles get their own 512-byte free list.
 */
import { PublicKey } from '@solana/web3.js';

import {
  isNil,
  readPubkey,
  readU32,
  readU8,
  view,
} from './_read.js';
import {
  RISK_PROFILE_DEPOSITOR_SEAT_SIZE,
  RISK_PROFILE_ORDER_REF_SIZE,
  RISK_PROFILE_SIZE,
  RiskProfile,
  RiskProfileDepositorSeat,
  RiskProfileOrderRef,
  decodeRiskProfile,
  decodeRiskProfileDepositorSeat,
  decodeRiskProfileOrderRef,
} from './riskProfile.js';
import { walkDescending, walkFreeList } from './trees.js';

export const GLOBAL_VAULT_FIXED_SIZE = 320;
export const GLOBAL_VAULT_DISCRIMINANT = 0x79_64_65_6c_74_61_56_61n; // "ydeltaVa"

export interface GlobalVaultHeader {
  mint: PublicKey;
  globalVaultAdmin: PublicKey;
  integrationPool: PublicKey;
  integrationAccount: PublicKey;
  globalVaultSigner: PublicKey;
  lendingPool: PublicKey;
  riskProfilesRootIndex: number;
  claimedSeatsRootIndex: number;
  marketOrdersRootIndex: number;
  profileFreeListHeadIndex: number;
  nodeFreeListHeadIndex: number;
  numBytesAllocated: number;
  riskProfileCount: number;
  globalVaultSignerBump: number;
  version: number;
  claimedSeatCount: number;
  openOrderCount: number;
  pendingGlobalVaultAdmin: PublicKey;
  isPaused: boolean;
  /**
   * Monotonic counter for the next `profile_id` to assign on
   * `CreateRiskProfile`. Bumped on every successful create; **never**
   * decremented on `RemoveRiskProfile`, so this value is the id the
   * next create will receive (callers can pre-read it to predict the
   * assignment).
   */
  nextProfileId: number;
}

export interface GlobalVault {
  header: GlobalVaultHeader;
  riskProfiles: Array<{ index: number; profile: RiskProfile }>;
  depositorSeats: Array<{ index: number; seat: RiskProfileDepositorSeat }>;
  marketOrders: Array<{ index: number; order: RiskProfileOrderRef }>;
}

/* ── Header ──────────────────────────────────────────────── */

export function decodeGlobalVaultHeader(data: Uint8Array | Buffer): GlobalVaultHeader {
  if (data.byteLength < GLOBAL_VAULT_FIXED_SIZE) {
    throw new RangeError(`decodeGlobalVaultHeader: account too small`);
  }
  const dv = view(data);
  const disc = dv.getBigUint64(0, true);
  if (disc !== GLOBAL_VAULT_DISCRIMINANT) {
    throw new Error(
      `decodeGlobalVaultHeader: bad discriminator 0x${disc.toString(16)} (expected 0x${GLOBAL_VAULT_DISCRIMINANT.toString(16)})`,
    );
  }
  return {
    mint: readPubkey(dv, 8),
    globalVaultAdmin: readPubkey(dv, 40),
    integrationPool: readPubkey(dv, 72),
    integrationAccount: readPubkey(dv, 104),
    globalVaultSigner: readPubkey(dv, 136),
    lendingPool: readPubkey(dv, 168),
    riskProfilesRootIndex: readU32(dv, 200),
    claimedSeatsRootIndex: readU32(dv, 204),
    marketOrdersRootIndex: readU32(dv, 208),
    profileFreeListHeadIndex: readU32(dv, 212),
    nodeFreeListHeadIndex: readU32(dv, 216),
    numBytesAllocated: readU32(dv, 220),
    riskProfileCount: readU8(dv, 224),
    globalVaultSignerBump: readU8(dv, 225),
    version: readU8(dv, 226),
    claimedSeatCount: readU32(dv, 228),
    openOrderCount: readU32(dv, 232),
    pendingGlobalVaultAdmin: readPubkey(dv, 240),
    isPaused: readU8(dv, 304) !== 0,
    nextProfileId: readU8(dv, 305),
  };
}

/* ── Tree walkers ────────────────────────────────────────── */

export function vaultDynamicRegion(data: Uint8Array | Buffer): DataView {
  const sub = data.subarray
    ? data.subarray(GLOBAL_VAULT_FIXED_SIZE)
    : new Uint8Array(data.buffer, data.byteOffset + GLOBAL_VAULT_FIXED_SIZE);
  return new DataView(sub.buffer, sub.byteOffset, sub.byteLength);
}

export function* iterRiskProfiles(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; profile: RiskProfile }> {
  const header = rootIndex === undefined ? decodeGlobalVaultHeader(data) : null;
  const root = rootIndex ?? header!.riskProfilesRootIndex;
  if (isNil(root)) return;
  const dynamic = vaultDynamicRegion(data);
  for (const node of walkDescending(dynamic, root, RISK_PROFILE_SIZE)) {
    yield { index: node.index, profile: decodeRiskProfile(node.payload) };
  }
}

export function* iterDepositorSeats(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; seat: RiskProfileDepositorSeat }> {
  const header = rootIndex === undefined ? decodeGlobalVaultHeader(data) : null;
  const root = rootIndex ?? header!.claimedSeatsRootIndex;
  if (isNil(root)) return;
  const dynamic = vaultDynamicRegion(data);
  for (const node of walkDescending(dynamic, root, RISK_PROFILE_DEPOSITOR_SEAT_SIZE)) {
    yield { index: node.index, seat: decodeRiskProfileDepositorSeat(node.payload) };
  }
}

export function* iterMarketOrders(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; order: RiskProfileOrderRef }> {
  const header = rootIndex === undefined ? decodeGlobalVaultHeader(data) : null;
  const root = rootIndex ?? header!.marketOrdersRootIndex;
  if (isNil(root)) return;
  const dynamic = vaultDynamicRegion(data);
  for (const node of walkDescending(dynamic, root, RISK_PROFILE_ORDER_REF_SIZE)) {
    yield { index: node.index, order: decodeRiskProfileOrderRef(node.payload) };
  }
}

export function iterProfileFreeList(data: Uint8Array | Buffer, headIndex?: number): Iterable<number> {
  const head = headIndex ?? decodeGlobalVaultHeader(data).profileFreeListHeadIndex;
  return walkFreeList(vaultDynamicRegion(data), head);
}

export function iterNodeFreeList(data: Uint8Array | Buffer, headIndex?: number): Iterable<number> {
  const head = headIndex ?? decodeGlobalVaultHeader(data).nodeFreeListHeadIndex;
  return walkFreeList(vaultDynamicRegion(data), head);
}

/* ── Full decode ─────────────────────────────────────────── */

export function decodeGlobalVault(data: Uint8Array | Buffer): GlobalVault {
  const header = decodeGlobalVaultHeader(data);
  return {
    header,
    riskProfiles: [...iterRiskProfiles(data, header.riskProfilesRootIndex)],
    depositorSeats: [...iterDepositorSeats(data, header.claimedSeatsRootIndex)],
    marketOrders: [...iterMarketOrders(data, header.marketOrdersRootIndex)],
  };
}
