/**
 * `ClaimedSeat` — per-trader bookkeeping tree-node payload inside a market's
 * dynamic region. 144 bytes (= MARKET_BLOCK_PAYLOAD_SIZE). Lives behind a
 * 16-byte RBNode header. Tree key: `Ord on (owner_kind, owner, risk_profile_id)`.
 *
 * Layout:
 *   @0    32   Pubkey  owner
 *   @32   16   u128    debt_withdrawable_shares
 *   @48   16   u128    debt_encumbered_shares
 *   @64   16   u128    collateral_withdrawable_shares
 *   @80   16   u128    collateral_encumbered_shares
 *   @96   4    u32     open_borrow_count
 *   @100  4    u32     open_lend_count
 *   @104  1    u8      owner_kind  (OwnerKind enum: User=0, RiskProfile=1)
 *   @105  1    u8      risk_profile_id
 *   @106  6    u8[6]   _padding
 *   @112  32   u64[4]  _reserved
 */
import { PublicKey } from '@solana/web3.js';

import { OwnerKind } from '../types.js';
import { readPubkey, readU128, readU32, readU8 } from './_read.js';

export const CLAIMED_SEAT_SIZE = 144;

export interface ClaimedSeat {
  owner: PublicKey;
  debtWithdrawableShares: bigint;
  debtEncumberedShares: bigint;
  collateralWithdrawableShares: bigint;
  collateralEncumberedShares: bigint;
  openBorrowCount: number;
  openLendCount: number;
  ownerKind: OwnerKind;
  riskProfileId: number;
}

/** Decode a `ClaimedSeat` from a 144-byte payload `DataView` (post-RBNode header). */
export function decodeClaimedSeat(payload: DataView): ClaimedSeat {
  return {
    owner: readPubkey(payload, 0),
    debtWithdrawableShares: readU128(payload, 32),
    debtEncumberedShares: readU128(payload, 48),
    collateralWithdrawableShares: readU128(payload, 64),
    collateralEncumberedShares: readU128(payload, 80),
    openBorrowCount: readU32(payload, 96),
    openLendCount: readU32(payload, 100),
    ownerKind: readU8(payload, 104) as OwnerKind,
    riskProfileId: readU8(payload, 105),
  };
}
