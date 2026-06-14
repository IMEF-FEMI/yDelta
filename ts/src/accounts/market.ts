// MarketFixed 512-byte header + dynamic region (asks tree, claimed-seats tree,
// matched-loans tree, shared 160-byte free list).
//
// Byte offsets live in `./layout.ts` (`MarketFixedOffsets`,
// `FeeConfigOffsets`). Keep raw reads here using those constants — no
// magic numbers.
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
import { FeeConfigOffsets, MarketFixedOffsets } from './layout.js';
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
  liquidationKeeperBps: number;
  liquidationProtocolBps: number;
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
  bidsRootIndex: number;
  bidsBestIndex: number;
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
}

export interface Market {
  header: MarketHeader;
  bids: Array<{ index: number; order: RestingOrder }>;
  asks: Array<{ index: number; order: RestingOrder }>;
  claimedSeats: Array<{ index: number; seat: ClaimedSeat }>;
  matchedLoans: Array<{ index: number; loan: MatchedLoan }>;
}

// v1 FeeConfig (state/market.rs): `curator_fee_bps` moved to the sub-vault
// (D3b) and `ltv_buffer_bps` was removed (D17) — both are gone here, and
// the liquidation fields shifted down to offset 6/8. See `FeeConfigOffsets`.
function decodeFeeConfig(dv: DataView, base: number): FeeConfig {
  return {
    protocolFeeBpsFloor: readU16(dv, base + FeeConfigOffsets.PROTOCOL_FEE_BPS_FLOOR),
    originationBps: readU16(dv, base + FeeConfigOffsets.ORIGINATION_BPS),
    curatorSplitBps: readU16(dv, base + FeeConfigOffsets.CURATOR_SPLIT_BPS),
    liquidationKeeperBps: readU16(dv, base + FeeConfigOffsets.LIQUIDATION_KEEPER_BPS),
    liquidationProtocolBps: readU16(dv, base + FeeConfigOffsets.LIQUIDATION_PROTOCOL_BPS),
    gracePeriodSeconds: readU32(dv, base + FeeConfigOffsets.GRACE_PERIOD_SECONDS),
  };
}

export function decodeMarketHeader(data: Uint8Array | Buffer): MarketHeader {
  if (data.byteLength < MARKET_FIXED_SIZE) {
    throw new RangeError(`decodeMarketHeader: account too small (${data.byteLength})`);
  }
  const dv = view(data);
  const disc = dv.getBigUint64(MarketFixedOffsets.DISCRIMINATOR, true);
  if (disc !== MARKET_FIXED_DISCRIMINANT) {
    throw new Error(
      `decodeMarketHeader: bad discriminator 0x${disc.toString(16)} (expected 0x${MARKET_FIXED_DISCRIMINANT.toString(16)})`,
    );
  }
  return {
    version: readU8(dv, MarketFixedOffsets.VERSION),
    debtMintDecimals: readU8(dv, MarketFixedOffsets.DEBT_MINT_DECIMALS),
    collateralMintDecimals: readU8(dv, MarketFixedOffsets.COLLATERAL_MINT_DECIMALS),
    debtVaultBump: readU8(dv, MarketFixedOffsets.DEBT_VAULT_BUMP),
    collateralVaultBump: readU8(dv, MarketFixedOffsets.COLLATERAL_VAULT_BUMP),
    debtMint: readPubkey(dv, MarketFixedOffsets.DEBT_MINT),
    collateralMint: readPubkey(dv, MarketFixedOffsets.COLLATERAL_MINT),
    debtVault: readPubkey(dv, MarketFixedOffsets.DEBT_VAULT),
    collateralVault: readPubkey(dv, MarketFixedOffsets.COLLATERAL_VAULT),
    accumulatedProtocolFeeShares: readU128(dv, MarketFixedOffsets.ACCUMULATED_PROTOCOL_FEE_SHARES),
    orderSequenceNumber: readU64(dv, MarketFixedOffsets.ORDER_SEQUENCE_NUMBER),
    matchedLoanSequence: readU64(dv, MarketFixedOffsets.MATCHED_LOAN_SEQUENCE),
    numBytesAllocated: readU32(dv, MarketFixedOffsets.NUM_BYTES_ALLOCATED),
    bidsRootIndex: readU32(dv, MarketFixedOffsets.BIDS_ROOT_INDEX),
    bidsBestIndex: readU32(dv, MarketFixedOffsets.BIDS_BEST_INDEX),
    asksRootIndex: readU32(dv, MarketFixedOffsets.ASKS_ROOT_INDEX),
    asksBestIndex: readU32(dv, MarketFixedOffsets.ASKS_BEST_INDEX),
    claimedSeatsRootIndex: readU32(dv, MarketFixedOffsets.CLAIMED_SEATS_ROOT_INDEX),
    matchedLoansRootIndex: readU32(dv, MarketFixedOffsets.MATCHED_LOANS_ROOT_INDEX),
    freeListHeadIndex: readU32(dv, MarketFixedOffsets.FREE_LIST_HEAD_INDEX),
    positionCount: readU32(dv, MarketFixedOffsets.POSITION_COUNT),
    feeConfig: decodeFeeConfig(dv, MarketFixedOffsets.FEE_CONFIG),
    lenderIntegrationAccount: readPubkey(dv, MarketFixedOffsets.LENDER_INTEGRATION_ACCOUNT),
    borrowerIntegrationAccount: readPubkey(dv, MarketFixedOffsets.BORROWER_INTEGRATION_ACCOUNT),
    debtLendingPool: readPubkey(dv, MarketFixedOffsets.DEBT_LENDING_POOL),
    collateralLendingPool: readPubkey(dv, MarketFixedOffsets.COLLATERAL_LENDING_POOL),
    marginfiGroup: readPubkey(dv, MarketFixedOffsets.MARGINFI_GROUP),
    marketSigner: readPubkey(dv, MarketFixedOffsets.MARKET_SIGNER),
    marketSignerBump: readU8(dv, MarketFixedOffsets.MARKET_SIGNER_BUMP),
    lenderIntegrationAccountBump: readU8(dv, MarketFixedOffsets.LENDER_INTEGRATION_ACCOUNT_BUMP),
    borrowerIntegrationAccountBump: readU8(dv, MarketFixedOffsets.BORROWER_INTEGRATION_ACCOUNT_BUMP),
    admin: readPubkey(dv, MarketFixedOffsets.ADMIN),
    pendingAdmin: readPubkey(dv, MarketFixedOffsets.PENDING_ADMIN),
    isPaused: readU8(dv, MarketFixedOffsets.IS_PAUSED) !== 0,
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

// Bid tree (restored in v1 D6 — borrower residuals with residualMode = Rest
// live here). RestingOrder Ord makes the bid tree max = HIGHEST rate (the
// borrower willing to pay most), so descending traversal yields best-bid
// first — the same priority direction iterAsks gives for the cheapest ask.
export function* iterBids(
  data: Uint8Array | Buffer,
  rootIndex?: number,
): Generator<{ index: number; order: RestingOrder }> {
  const root = rootIndex ?? decodeMarketHeader(data).bidsRootIndex;
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
    bids: [...iterBids(data, header.bidsRootIndex)],
    asks: [...iterAsks(data, header.asksRootIndex)],
    claimedSeats: [...iterClaimedSeats(data, header.claimedSeatsRootIndex)],
    matchedLoans: [...iterMatchedLoans(data, header.matchedLoansRootIndex)],
  };
}
