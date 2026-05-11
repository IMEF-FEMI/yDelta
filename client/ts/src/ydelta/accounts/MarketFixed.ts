import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import {
  MARKET_FIXED_DISCRIMINANT,
  MARKET_FIXED_SIZE,
  readU64Le,
} from '../../utils/discriminator';
import { FeeConfig } from '../types/FeeConfig';

export { MARKET_FIXED_SIZE };

export type MarketFixed = {
  discriminator: bigint;
  version: number;
  debtMintDecimals: number;
  collateralMintDecimals: number;
  debtVaultBump: number;
  collateralVaultBump: number;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  debtVault: PublicKey;
  collateralVault: PublicKey;
  /** marginfi adapter-share units (fp48 u128), NOT token atoms */
  accumulatedProtocolFeeShares: BN;
  /** monotonically increasing */
  orderSequenceNumber: BN;
  /** monotonically increasing */
  matchedLoanSequence: BN;
  /** bytes */
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
};

function readU128Le(buf: Buffer, offset: number): BN {
  return new BN(buf.subarray(offset, offset + 16), 'le');
}

function readU64BN(buf: Buffer, offset: number): BN {
  return new BN(buf.subarray(offset, offset + 8), 'le');
}

function decodeFeeConfig(buf: Buffer, offset: number): FeeConfig {
  // Offsets 14..16 and 20..24 are alignment padding on the on-chain struct
  // (`_padding_for_u32` and `_padding_tail`); skipped here intentionally.
  return {
    protocolFeeBpsFloor: buf.readUInt16LE(offset + 0),
    originationBps: buf.readUInt16LE(offset + 2),
    curatorSplitBps: buf.readUInt16LE(offset + 4),
    curatorFeeBps: buf.readUInt16LE(offset + 6),
    liquidationKeeperBps: buf.readUInt16LE(offset + 8),
    liquidationProtocolBps: buf.readUInt16LE(offset + 10),
    ltvBufferBps: buf.readUInt16LE(offset + 12),
    gracePeriodSeconds: buf.readUInt32LE(offset + 16),
  };
}

export function decodeMarketFixed(buf: Buffer): MarketFixed {
  if (buf.length < MARKET_FIXED_SIZE) {
    throw new Error(`Market buffer too small: ${buf.length} < ${MARKET_FIXED_SIZE}`);
  }
  const discriminator = readU64Le(buf, 0);
  if (discriminator !== MARKET_FIXED_DISCRIMINANT) {
    throw new Error(
      `Market discriminator mismatch: 0x${discriminator.toString(16)} != 0x${MARKET_FIXED_DISCRIMINANT.toString(16)}`,
    );
  }
  // Offset map matches `programs/ydelta/src/state/market.rs::MarketFixed`.
  return {
    discriminator,
    version: buf.readUInt8(8),
    debtMintDecimals: buf.readUInt8(9),
    collateralMintDecimals: buf.readUInt8(10),
    debtVaultBump: buf.readUInt8(11),
    collateralVaultBump: buf.readUInt8(12),
    debtMint: new PublicKey(buf.subarray(16, 48)),
    collateralMint: new PublicKey(buf.subarray(48, 80)),
    debtVault: new PublicKey(buf.subarray(80, 112)),
    collateralVault: new PublicKey(buf.subarray(112, 144)),
    accumulatedProtocolFeeShares: readU128Le(buf, 144),
    orderSequenceNumber: readU64BN(buf, 160),
    matchedLoanSequence: readU64BN(buf, 168),
    numBytesAllocated: buf.readUInt32LE(176),
    bidsRootIndex: buf.readUInt32LE(180),
    bidsBestIndex: buf.readUInt32LE(184),
    asksRootIndex: buf.readUInt32LE(188),
    asksBestIndex: buf.readUInt32LE(192),
    claimedSeatsRootIndex: buf.readUInt32LE(196),
    matchedLoansRootIndex: buf.readUInt32LE(200),
    freeListHeadIndex: buf.readUInt32LE(204),
    positionCount: buf.readUInt32LE(208),
    feeConfig: decodeFeeConfig(buf, 212),
    // 212 + 24 (FeeConfig) + 4 (padding) = 240 — integration region starts.
    lenderIntegrationAccount: new PublicKey(buf.subarray(240, 272)),
    borrowerIntegrationAccount: new PublicKey(buf.subarray(272, 304)),
    debtLendingPool: new PublicKey(buf.subarray(304, 336)),
    collateralLendingPool: new PublicKey(buf.subarray(336, 368)),
    marginfiGroup: new PublicKey(buf.subarray(368, 400)),
    marketSigner: new PublicKey(buf.subarray(400, 432)),
    marketSignerBump: buf.readUInt8(432),
    lenderIntegrationAccountBump: buf.readUInt8(433),
    borrowerIntegrationAccountBump: buf.readUInt8(434),
    // 432 + 8 (3 bumps + 5 padding) = 440 — admin region starts.
    admin: new PublicKey(buf.subarray(440, 472)),
    pendingAdmin: new PublicKey(buf.subarray(472, 504)),
    isPaused: buf.readUInt8(504) !== 0,
  };
}
