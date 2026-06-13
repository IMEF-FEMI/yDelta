import { Connection, PublicKey, TransactionInstruction } from '@solana/web3.js';

import { decodeMarketHeader, iterClaimedSeats } from '../accounts/market.js';
import { OwnerKind } from '../types.js';
import { claimSeatInstruction } from './claimSeat.js';

/**
 * Scan a market's `claimed_seats` RB-tree for a user-side seat owned by
 * `owner` (the canonical `OWNER_KIND_USER, risk_profile_id = 0` shape the
 * `deposit` ix's seat lookup requires). Pure — pass pre-fetched market
 * account data.
 *
 * Iteration is O(seats) worst case; we early-return on the first match,
 * so a market with a few hundred seats costs at most that many RB-node
 * reads (one comparison per node). Markets with tens of thousands of
 * seats should bind via account-level memcmp filters instead — out of
 * scope for the SDK helper.
 */
export function hasUserSeat(marketData: Uint8Array | Buffer, owner: PublicKey): boolean {
  const header = decodeMarketHeader(marketData);
  for (const { seat } of iterClaimedSeats(marketData, header.claimedSeatsRootIndex)) {
    if (
      seat.ownerKind === OwnerKind.User &&
      seat.subVaultId === 0 &&
      seat.owner.equals(owner)
    ) {
      return true;
    }
  }
  return false;
}

/**
 * Bundle helper for the "wallet user wants to deposit but has never
 * touched this market" path. Returns `[]` if `payer` already has a
 * user-side seat in `market`, otherwise returns
 * `[claimSeatInstruction({ payer, market })]` so the caller can prepend
 * it to their deposit tx and get the whole flow in a single signature.
 *
 * Two call shapes:
 *
 *   // Pre-fetched market data — preferred when the caller already
 *   // decoded the market for other reasons (e.g. quoting against asks).
 *   const ixs = claimSeatIfNeededInstructions({ market, payer, marketData });
 *
 *   // Connection mode — convenience for one-shot scripts / wallet UX
 *   // where the caller only has a Connection.
 *   const ixs = await claimSeatIfNeededInstructions({ market, payer, connection });
 *
 * Notes:
 *   - The seat lookup mirrors the on-chain `get_seat_index_with_hint`
 *     probe (`OWNER_KIND_USER, risk_profile_id = 0`). Risk-profile seats
 *     are addressed by a separate ix path; this helper is for user
 *     deposits only.
 *   - If the market account doesn't exist (RPC returns null), this
 *     helper falls through to "no seat" and returns the claim ix. The
 *     claim ix itself will then fail with a clear loader error so the
 *     caller gets a single, attributable failure mode.
 */
export interface ClaimSeatIfNeededArgs {
  market: PublicKey;
  payer: PublicKey;
  /**
   * Pre-fetched market account data. If supplied, the helper runs
   * synchronously and ignores `connection`.
   */
  marketData?: Uint8Array | Buffer;
  /**
   * Required when `marketData` is omitted. The helper fetches
   * `market` via `connection.getAccountInfo` to decide.
   */
  connection?: Connection;
}

export function claimSeatIfNeededInstructions(
  args: ClaimSeatIfNeededArgs & { marketData: Uint8Array | Buffer },
): TransactionInstruction[];
export function claimSeatIfNeededInstructions(
  args: ClaimSeatIfNeededArgs & { connection: Connection },
): Promise<TransactionInstruction[]>;
export function claimSeatIfNeededInstructions(
  args: ClaimSeatIfNeededArgs,
): TransactionInstruction[] | Promise<TransactionInstruction[]> {
  if (args.marketData !== undefined) {
    return hasUserSeat(args.marketData, args.payer)
      ? []
      : [claimSeatInstruction({ payer: args.payer, market: args.market })];
  }
  if (!args.connection) {
    throw new Error(
      'claimSeatIfNeededInstructions: provide either `marketData` or `connection`',
    );
  }
  return args.connection.getAccountInfo(args.market).then((info) => {
    if (info && hasUserSeat(info.data, args.payer)) {
      return [];
    }
    return [claimSeatInstruction({ payer: args.payer, market: args.market })];
  });
}
