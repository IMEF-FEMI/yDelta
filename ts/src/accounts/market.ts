// MarketFixed 512-byte header + dynamic region (asks tree, claimed-seats tree,
// matched-loans tree, shared 160-byte free list).
//
// Header byte map (from state/market.rs):
//   @0    u64    discriminator
//   @8    u8     version
//   @9    u8     debt_mint_decimals
//   @10   u8     collateral_mint_decimals
//   @11   u8     debt_vault_bump
//   @12   u8     collateral_vault_bump
//   @16   Pubkey debt_mint
//   @48   Pubkey collateral_mint
//   @80   Pubkey debt_vault
//   @112  Pubkey collateral_vault
//   @144  u128   accumulated_protocol_fee_shares
//   @160  u64    order_sequence_number
//   @168  u64    matched_loan_sequence
//   @176  u32    num_bytes_allocated
//   @188  u32    asks_root_index
//   @192  u32    asks_best_index
//   @196  u32    claimed_seats_root_index
//   @200  u32    matched_loans_root_index
//   @204  u32    free_list_head_index
//   @208  u32    position_count
//   @212  FeeConfig (24)
//   @240  Pubkey lender_integration_account
//   @272  Pubkey borrower_integration_account
//   @304  Pubkey debt_lending_pool
//   @336  Pubkey collateral_lending_pool
//   @368  Pubkey marginfi_group
//   @400  Pubkey market_signer
//   @432  u8     market_signer_bump
//   @433  u8     lender_integration_account_bump
//   @434  u8     borrower_integration_account_bump
//   @440  Pubkey admin
//   @472  Pubkey pending_admin
//   @504  u8     is_paused
//   @505  u8     fee_config_set
//
// FeeConfig (24 bytes):
//   +0  u16  protocol_fee_bps_floor
//   +2  u16  origination_bps
//   +4  u16  curator_split_bps
//   +6  u16  curator_fee_bps
//   +8  u16  liquidation_keeper_bps
//   +10 u16  liquidation_protocol_bps
//   +12 u16  ltv_buffer_bps
//   +16 u32  grace_period_seconds
import { PublicKey } from '@solana/web3.js';

import {
  isNil,
  readPubkey,
  readU128,
  readU16,
  readU32,
  readU64,
  readU8,
  view,
} from './_read.js';
import { CLAIMED_SEAT_SIZE, ClaimedSeat, decodeClaimedSeat } from './claimedSeat.js';
import { MATCHED_LOAN_SIZE, MatchedLoan, decodeMatchedLoan } from './matchedLoan.js';
import { RESTING_ORDER_SIZE, RestingOrder, decodeRestingOrder } from './restingOrder.js';
import { walkDescending, walkFreeList } from './trees.js';

export const MARKET_FIXED_SIZE = 512;
export const MARKET_BLOCK_PAYLOAD_SIZE = 144;
export const MARKET_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_4d_4bn; // "ydeltaMK"

export interface FeeConfig {
  protocolFeeBpsFloor: number;
  originationBps: number;
  curatorSplitBps: number;
  curatorFeeBps: number;
  liquidationKeeperBps: number;
  liquidationProtocolBps: number;
  ltvBufferBps: number;
  gracePeriodSeconds: number;
}

export interface MarketHeader {
  version: number;
  debtMintDecimals: number;
  collateralMintDecimals: number;
  debtVaultBump: number;
  collateralVaultBump: number;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  debtVault: PublicKey;
  collateralVault: PublicKey;
  accumulatedProtocolFeeShares: bigint;
  orderSequenceNumber: bigint;
  matchedLoanSequence: bigint;
  numBytesAllocated: number;
  asksRootIndex: number;
  asksBestIndex: number;
  claimedSeatsRootIndex: number;
  matchedLoansRootIndex: number;
  freeListHeadIndex: number;
  positionCount: number;
  feeConfig: FeeConfig;
  lenderIntegrationAccount: PublicKey;
  borrowerIntegrationAccount: PublicKey;
  debtLendingPool: PublicKey;
  collateralLendingPool: PublicKey;
  marginfiGroup: PublicKey;
  marketSigner: PublicKey;
  marketSignerBump: number;
  lenderIntegrationAccountBump: number;
  borrowerIntegrationAccountBump: number;
  admin: PublicKey;
  pendingAdmin: PublicKey;
  isPaused: boolean;
  feeConfigSet: boolean;
}

export interface Market {
  header: MarketHeader;
  asks: Array<{ index: number; order: RestingOrder }>;
  claimedSeats: Array<{ index: number; seat: ClaimedSeat }>;
  matchedLoans: Array<{ index: number; loan: MatchedLoan }>;
}

function decodeFeeConfig(dv: DataView, base: number): FeeConfig {
  return {
    protocolFeeBpsFloor: readU16(dv, base + 0),
    originationBps: readU16(dv, base + 2),
    curatorSplitBps: readU16(dv, base + 4),
    curatorFeeBps: readU16(dv, base + 6),
    liquidationKeeperBps: readU16(dv, base + 8),
    liquidationProtocolBps: readU16(dv, base + 10),
    ltvBufferBps: readU16(dv, base + 12),
    gracePeriodSeconds: readU32(dv, base + 16),
  };
}

export function decodeMarketHeader(data: Uint8Array | Buffer): MarketHeader {
  if (data.byteLength < MARKET_FIXED_SIZE) {
    throw new RangeError(`decodeMarketHeader: account too small (${data.byteLength})`);
  }
  const dv = view(data);
  const disc = dv.getBigUint64(0, true);
  if (disc !== MARKET_FIXED_DISCRIMINANT) {
    throw new Error(
      `decodeMarketHeader: bad discriminator 0x${disc.toString(16)} (expected 0x${MARKET_FIXED_DISCRIMINANT.toString(16)})`,
    );
  }
  return {
    version: readU8(dv, 8),
    debtMintDecimals: readU8(dv, 9),
    collateralMintDecimals: readU8(dv, 10),
    debtVaultBump: readU8(dv, 11),
    collateralVaultBump: readU8(dv, 12),
    debtMint: readPubkey(dv, 16),
    collateralMint: readPubkey(dv, 48),
    debtVault: readPubkey(dv, 80),
    collateralVault: readPubkey(dv, 112),
    accumulatedProtocolFeeShares: readU128(dv, 144),
    orderSequenceNumber: readU64(dv, 160),
    matchedLoanSequence: readU64(dv, 168),
    numBytesAllocated: readU32(dv, 176),
    asksRootIndex: readU32(dv, 188),
    asksBestIndex: readU32(dv, 192),
    claimedSeatsRootIndex: readU32(dv, 196),
    matchedLoansRootIndex: readU32(dv, 200),
    freeListHeadIndex: readU32(dv, 204),
    positionCount: readU32(dv, 208),
    feeConfig: decodeFeeConfig(dv, 212),
    lenderIntegrationAccount: readPubkey(dv, 240),
    borrowerIntegrationAccount: readPubkey(dv, 272),
    debtLendingPool: readPubkey(dv, 304),
    collateralLendingPool: readPubkey(dv, 336),
    marginfiGroup: readPubkey(dv, 368),
    marketSigner: readPubkey(dv, 400),
    marketSignerBump: readU8(dv, 432),
    lenderIntegrationAccountBump: readU8(dv, 433),
    borrowerIntegrationAccountBump: readU8(dv, 434),
    admin: readPubkey(dv, 440),
    pendingAdmin: readPubkey(dv, 472),
    isPaused: readU8(dv, 504) !== 0,
    feeConfigSet: readU8(dv, 505) !== 0,
  };
}

export function dynamicRegion(data: Uint8Array | Buffer): DataView {
  const sub = data.subarray
    ? data.subarray(MARKET_FIXED_SIZE)
    : new Uint8Array(data.buffer, data.byteOffset + MARKET_FIXED_SIZE);
  return new DataView(sub.buffer, sub.byteOffset, sub.byteLength);
}

// RestingOrder Ord is (rate_bps DESC, sequence DESC) → tree max = best ask;
// descending traversal yields priority order.
export function* iterAsks(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; order: RestingOrder }> {
  const root = rootIndex ?? decodeMarketHeader(data).asksRootIndex;
  if (isNil(root)) return;
  const dynamic = dynamicRegion(data);
  for (const node of walkDescending(dynamic, root, RESTING_ORDER_SIZE)) {
    yield { index: node.index, order: decodeRestingOrder(node.payload) };
  }
}

export function* iterClaimedSeats(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; seat: ClaimedSeat }> {
  const root = rootIndex ?? decodeMarketHeader(data).claimedSeatsRootIndex;
  if (isNil(root)) return;
  const dynamic = dynamicRegion(data);
  for (const node of walkDescending(dynamic, root, CLAIMED_SEAT_SIZE)) {
    yield { index: node.index, seat: decodeClaimedSeat(node.payload) };
  }
}

export function* iterMatchedLoans(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; loan: MatchedLoan }> {
  const root = rootIndex ?? decodeMarketHeader(data).matchedLoansRootIndex;
  if (isNil(root)) return;
  const dynamic = dynamicRegion(data);
  for (const node of walkDescending(dynamic, root, MATCHED_LOAN_SIZE)) {
    yield { index: node.index, loan: decodeMatchedLoan(node.payload) };
  }
}

export function iterFreeList(data: Uint8Array | Buffer, headIndex?: number): Iterable<number> {
  const head = headIndex ?? decodeMarketHeader(data).freeListHeadIndex;
  return walkFreeList(dynamicRegion(data), head);
}

// `dataIndex` is a byte offset into the dynamic region (same convention as
// RestingOrder.traderSeatIndex, MatchedLoan.{lender,borrower}_seat_index).
// Payload starts at dataIndex + 16 (past the RBNode header).
export function lookupClaimedSeatAt(
  data: Uint8Array | Buffer,
  dataIndex: number,
): ClaimedSeat | null {
  if (dataIndex === 0xffff_ffff) return null;
  const dynamic = dynamicRegion(data);
  const payloadOff = dataIndex + 16;
  if (payloadOff + CLAIMED_SEAT_SIZE > dynamic.byteLength) return null;
  const payload = new DataView(
    dynamic.buffer,
    dynamic.byteOffset + payloadOff,
    CLAIMED_SEAT_SIZE,
  );
  return decodeClaimedSeat(payload);
}

export function decodeMarket(data: Uint8Array | Buffer): Market {
  const header = decodeMarketHeader(data);
  return {
    header,
    asks: [...iterAsks(data, header.asksRootIndex)],
    claimedSeats: [...iterClaimedSeats(data, header.claimedSeatsRootIndex)],
    matchedLoans: [...iterMatchedLoans(data, header.matchedLoansRootIndex)],
  };
}
