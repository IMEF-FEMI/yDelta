/**
 * `claimSeatIfNeededInstructions` returns `[claimSeat]` when the payer has
 * no user-side seat in the market, and `[]` once the seat exists. The
 * helper is the recommended bundle for the "first deposit" flow.
 *
 * Coverage:
 *   - Pre-fetched marketData, no seats → returns [claimSeat]
 *   - Pre-fetched marketData, payer's seat present → returns []
 *   - Pre-fetched marketData, only a different owner's seat present →
 *     returns [claimSeat]
 *   - Pre-fetched marketData, payer present but as a SubVault seat
 *     (wrong ownerKind) → returns [claimSeat]
 *   - Missing both marketData and connection → throws
 *
 * The single-node RB-tree we synthesise here mirrors the on-disk layout
 * documented in `accounts/trees.ts:5-13`: a 16-byte node header
 * (left/right/parent = NIL, color = Black) followed by the 144-byte
 * `ClaimedSeat` payload. v1 `ClaimedSeat` keys the seat by
 * `(ownerKind, owner, subVaultId)` where `subVaultId` is a u16 at byte
 * offset 106 of the payload.
 */
import { describe, expect, it } from 'vitest';
import { PublicKey } from '@solana/web3.js';

import {
  CLAIMED_SEAT_SIZE,
  InstructionTag,
  MARKET_FIXED_SIZE,
  claimSeatIfNeededInstructions,
  hasUserSeat,
} from '../src/index.js';
import { NIL_INDEX } from '../src/accounts/_read.js';

const MARKET_DISC = 0x79_64_65_6c_74_61_4d_4bn; // "ydeltaMK"
const NODE_HEADER_BYTES = 16;

const PAYER = new PublicKey('11111111111111111111111111111112');
const OTHER = new PublicKey('11111111111111111111111111111113');
const MARKET = new PublicKey('CYf9nJB7eJYqVRm6ucMcRYrwvtT6mzS5HHKkc4dHHGxk');

/** MarketFixed header + 1-page dynamic region. */
function emptyMarket(rootIndex: number = NIL_INDEX): Uint8Array {
  const buf = new Uint8Array(MARKET_FIXED_SIZE + 1024);
  const dv = new DataView(buf.buffer);
  // Discriminator @0.
  dv.setBigUint64(0, MARKET_DISC, true);
  // claimed_seats_root_index @196.
  dv.setUint32(196, rootIndex, true);
  // Other tree roots → NIL so iter*() helpers don't try to walk them.
  dv.setUint32(188, NIL_INDEX, true); // asks_root_index
  dv.setUint32(200, NIL_INDEX, true); // matched_loans_root_index
  dv.setUint32(204, NIL_INDEX, true); // free_list_head_index
  return buf;
}

/**
 * Plant a single-node ClaimedSeat tree at byte offset 0 of the dynamic
 * region (so the tree's `claimed_seats_root_index = 0`). Returns the
 * mutated buffer.
 */
function plantSingleSeat(
  buf: Uint8Array,
  owner: PublicKey,
  ownerKind: number,
  subVaultId: number,
): Uint8Array {
  const dynStart = MARKET_FIXED_SIZE;
  const nodeOff = 0; // root at dynamic offset 0
  const dv = new DataView(buf.buffer);

  // Node header: left/right/parent = NIL, color = Black, payload_type = 0.
  dv.setUint32(dynStart + nodeOff + 0, NIL_INDEX, true); // left
  dv.setUint32(dynStart + nodeOff + 4, NIL_INDEX, true); // right
  dv.setUint32(dynStart + nodeOff + 8, NIL_INDEX, true); // parent
  dv.setUint8(dynStart + nodeOff + 12, 0); // color = Black
  dv.setUint8(dynStart + nodeOff + 13, 0); // payload_type
  // (14..16 padding stays 0)

  // ClaimedSeat payload @ +16.
  const payloadOff = dynStart + nodeOff + NODE_HEADER_BYTES;
  buf.set(owner.toBuffer(), payloadOff + 0); // owner
  // (debt/collateral shares and counts default to 0 — fine)
  dv.setUint8(payloadOff + 104, ownerKind); // owner_kind
  dv.setUint16(payloadOff + 106, subVaultId, true); // sub_vault_id (u16)

  // Sanity: payload must fit before buffer end.
  if (payloadOff + CLAIMED_SEAT_SIZE > buf.byteLength) {
    throw new Error('test buffer too small');
  }

  // Point the market header at this node as the tree root.
  dv.setUint32(196, nodeOff, true);
  return buf;
}

describe('claimSeatIfNeededInstructions (sync / marketData mode)', () => {
  it('returns [claimSeat] when the market has no seats at all', () => {
    const marketData = emptyMarket();
    const ixs = claimSeatIfNeededInstructions({
      market: MARKET,
      payer: PAYER,
      marketData,
    });
    expect(ixs).toHaveLength(1);
    expect(ixs[0].data[0]).toBe(InstructionTag.ClaimSeat);
    expect(ixs[0].keys[0].pubkey.equals(PAYER)).toBe(true);
    expect(ixs[0].keys[0].isSigner).toBe(true);
  });

  it('returns [] when the payer already has a user-side seat', () => {
    const buf = emptyMarket();
    plantSingleSeat(buf, PAYER, /* ownerKind=User */ 0, /* subVaultId */ 0);
    expect(hasUserSeat(buf, PAYER)).toBe(true);

    const ixs = claimSeatIfNeededInstructions({
      market: MARKET,
      payer: PAYER,
      marketData: buf,
    });
    expect(ixs).toHaveLength(0);
  });

  it('returns [claimSeat] when only another wallet has a seat', () => {
    const buf = emptyMarket();
    plantSingleSeat(buf, OTHER, 0, 0);
    expect(hasUserSeat(buf, PAYER)).toBe(false);

    const ixs = claimSeatIfNeededInstructions({
      market: MARKET,
      payer: PAYER,
      marketData: buf,
    });
    expect(ixs).toHaveLength(1);
    expect(ixs[0].data[0]).toBe(InstructionTag.ClaimSeat);
  });

  it('returns [claimSeat] when the payer has a SubVault seat but no user seat', () => {
    const buf = emptyMarket();
    // Same owner, but ownerKind = SubVault (1) — deposit's seat probe
    // requires ownerKind = User (0), so this should NOT count.
    plantSingleSeat(buf, PAYER, /* ownerKind=SubVault */ 1, /* subVaultId */ 7);
    expect(hasUserSeat(buf, PAYER)).toBe(false);

    const ixs = claimSeatIfNeededInstructions({
      market: MARKET,
      payer: PAYER,
      marketData: buf,
    });
    expect(ixs).toHaveLength(1);
  });
});

describe('claimSeatIfNeededInstructions (error path)', () => {
  it('throws when called without marketData or connection', () => {
    expect(() =>
      claimSeatIfNeededInstructions({ market: MARKET, payer: PAYER } as never),
    ).toThrow(/provide either `marketData` or `connection`/);
  });
});
