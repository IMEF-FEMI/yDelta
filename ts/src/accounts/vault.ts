/**
 * `GlobalVaultFixed` (320-byte header) + dynamic region holding three
 * RB-trees: `sub_vaults` (512-byte blocks), `claimed_seats`
 * (SubVaultDepositorSeat — 160-byte blocks), `market_orders`
 * (SubVaultOrderRef — 160-byte blocks). The latter two share a
 * 160-byte free list; sub-vaults get their own 512-byte free list.
 */
import { PublicKey } from '@solana/web3.js';

import {
  isNil,
  readPubkey,
  readU16,
  readU32,
  readU8,
  view,
} from './_read.js';
import {
  SUB_VAULT_DEPOSITOR_SEAT_SIZE,
  SUB_VAULT_ORDER_REF_SIZE,
  SUB_VAULT_SIZE,
  SubVault,
  SubVaultDepositorSeat,
  SubVaultOrderRef,
  decodeSubVault,
  decodeSubVaultDepositorSeat,
  decodeSubVaultOrderRef,
} from './subVault.js';
import { walkDescending, walkFreeList } from './trees.js';

export const GLOBAL_VAULT_FIXED_SIZE = 320;
export const GLOBAL_VAULT_DISCRIMINANT = 0x79_64_65_6c_74_61_56_61n; // "ydeltaVa"

export interface GlobalVaultHeader {
  mint: PublicKey;
  globalVaultAdmin: PublicKey;
  integrationPool: PublicKey;
  integrationAccount: PublicKey;
  globalVaultSigner: PublicKey;
  /** Marginfi bank this vault is keyed to (the vault PDA is `[b"vault", lendingPool]`). */
  lendingPool: PublicKey;
  subVaultsRootIndex: number;
  claimedSeatsRootIndex: number;
  marketOrdersRootIndex: number;
  subVaultFreeListHeadIndex: number;
  nodeFreeListHeadIndex: number;
  numBytesAllocated: number;
  subVaultCount: number;
  globalVaultSignerBump: number;
  version: number;
  claimedSeatCount: number;
  openOrderCount: number;
  pendingGlobalVaultAdmin: PublicKey;
  isPaused: boolean;
  /**
   * Monotonic counter for the next `sub_vault_id` to assign on
   * `CreatePoolSubVault` / `CreatePrivateSubVault`. Bumped on every
   * successful create; **never** decremented on `RemoveSubVault`, so
   * this value is the id the next create will receive (callers can
   * pre-read it to predict the assignment). Starts at 1; 0 is the
   * sentinel/invalid id.
   */
  nextSubVaultId: number;
}

export interface GlobalVault {
  header: GlobalVaultHeader;
  subVaults: Array<{ index: number; subVault: SubVault }>;
  depositorSeats: Array<{ index: number; seat: SubVaultDepositorSeat }>;
  marketOrders: Array<{ index: number; order: SubVaultOrderRef }>;
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
    subVaultsRootIndex: readU32(dv, 200),
    claimedSeatsRootIndex: readU32(dv, 204),
    marketOrdersRootIndex: readU32(dv, 208),
    subVaultFreeListHeadIndex: readU32(dv, 212),
    nodeFreeListHeadIndex: readU32(dv, 216),
    numBytesAllocated: readU32(dv, 220),
    subVaultCount: readU16(dv, 224),
    globalVaultSignerBump: readU8(dv, 226),
    version: readU8(dv, 227),
    claimedSeatCount: readU32(dv, 228),
    openOrderCount: readU32(dv, 232),
    pendingGlobalVaultAdmin: readPubkey(dv, 240),
    isPaused: readU8(dv, 304) !== 0,
    nextSubVaultId: readU16(dv, 306),
  };
}

/* ── Tree walkers ────────────────────────────────────────── */

export function vaultDynamicRegion(data: Uint8Array | Buffer): DataView {
  const sub = data.subarray
    ? data.subarray(GLOBAL_VAULT_FIXED_SIZE)
    : new Uint8Array(data.buffer, data.byteOffset + GLOBAL_VAULT_FIXED_SIZE);
  return new DataView(sub.buffer, sub.byteOffset, sub.byteLength);
}

export function* iterSubVaults(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; subVault: SubVault }> {
  const header = rootIndex === undefined ? decodeGlobalVaultHeader(data) : null;
  const root = rootIndex ?? header!.subVaultsRootIndex;
  if (isNil(root)) return;
  const dynamic = vaultDynamicRegion(data);
  for (const node of walkDescending(dynamic, root, SUB_VAULT_SIZE)) {
    yield { index: node.index, subVault: decodeSubVault(node.payload) };
  }
}

export function* iterDepositorSeats(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; seat: SubVaultDepositorSeat }> {
  const header = rootIndex === undefined ? decodeGlobalVaultHeader(data) : null;
  const root = rootIndex ?? header!.claimedSeatsRootIndex;
  if (isNil(root)) return;
  const dynamic = vaultDynamicRegion(data);
  for (const node of walkDescending(dynamic, root, SUB_VAULT_DEPOSITOR_SEAT_SIZE)) {
    yield { index: node.index, seat: decodeSubVaultDepositorSeat(node.payload) };
  }
}

export function* iterMarketOrders(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; order: SubVaultOrderRef }> {
  const header = rootIndex === undefined ? decodeGlobalVaultHeader(data) : null;
  const root = rootIndex ?? header!.marketOrdersRootIndex;
  if (isNil(root)) return;
  const dynamic = vaultDynamicRegion(data);
  for (const node of walkDescending(dynamic, root, SUB_VAULT_ORDER_REF_SIZE)) {
    yield { index: node.index, order: decodeSubVaultOrderRef(node.payload) };
  }
}

export function iterSubVaultFreeList(data: Uint8Array | Buffer, headIndex?: number): Iterable<number> {
  const head = headIndex ?? decodeGlobalVaultHeader(data).subVaultFreeListHeadIndex;
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
    subVaults: [...iterSubVaults(data, header.subVaultsRootIndex)],
    depositorSeats: [...iterDepositorSeats(data, header.claimedSeatsRootIndex)],
    marketOrders: [...iterMarketOrders(data, header.marketOrdersRootIndex)],
  };
}
