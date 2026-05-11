import { PublicKey } from '@solana/web3.js';
import {
  USER_ACCOUNT_FIXED_DISCRIMINANT,
  USER_ACCOUNT_FIXED_SIZE,
  readU64Le,
} from '../../utils/discriminator';

export { USER_ACCOUNT_FIXED_SIZE };

export type UserAccountFixed = {
  discriminator: bigint;
  owner: PublicKey;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  vaultPositionsRootIndex: number;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  marketPositionsRootIndex: number;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  openLoansRootIndex: number;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  freeListHeadIndex: number;
  /** bytes */
  numBytesAllocated: number;
  vaultPositionCount: number;
  marketPositionCount: number;
  openLoanCount: number;
  bump: number;
  version: number;
};

export function decodeUserAccountFixed(buf: Buffer): UserAccountFixed {
  if (buf.length < USER_ACCOUNT_FIXED_SIZE) {
    throw new Error(`UserAccount buffer too small: ${buf.length} < ${USER_ACCOUNT_FIXED_SIZE}`);
  }
  const discriminator = readU64Le(buf, 0);
  if (discriminator !== USER_ACCOUNT_FIXED_DISCRIMINANT) {
    throw new Error(
      `UserAccount discriminator mismatch: 0x${discriminator.toString(16)} != 0x${USER_ACCOUNT_FIXED_DISCRIMINANT.toString(16)}`,
    );
  }
  return {
    discriminator,
    owner: new PublicKey(buf.subarray(8, 40)),
    vaultPositionsRootIndex: buf.readUInt32LE(40),
    marketPositionsRootIndex: buf.readUInt32LE(44),
    openLoansRootIndex: buf.readUInt32LE(48),
    freeListHeadIndex: buf.readUInt32LE(52),
    numBytesAllocated: buf.readUInt32LE(56),
    vaultPositionCount: buf.readUInt16LE(60),
    marketPositionCount: buf.readUInt16LE(62),
    openLoanCount: buf.readUInt32LE(64),
    bump: buf.readUInt8(68),
    version: buf.readUInt8(69),
  };
}
