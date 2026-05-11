import { PublicKey } from '@solana/web3.js';
import {
  GLOBAL_VAULT_FIXED_DISCRIMINANT,
  GLOBAL_VAULT_FIXED_SIZE,
  readU64Le,
} from '../../utils/discriminator';

export { GLOBAL_VAULT_FIXED_SIZE };

export type GlobalVaultFixed = {
  discriminator: bigint;
  mint: PublicKey;
  globalVaultAdmin: PublicKey;
  integrationPool: PublicKey;
  integrationAccount: PublicKey;
  globalVaultSigner: PublicKey;
  lendingPool: PublicKey;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  riskProfilesRootIndex: number;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  claimedSeatsRootIndex: number;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  marketOrdersRootIndex: number;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  profileFreeListHeadIndex: number;
  /** DataIndex (u32 byte-offset into dynamic region; NIL = u32::MAX) */
  nodeFreeListHeadIndex: number;
  /** bytes */
  numBytesAllocated: number;
  riskProfileCount: number;
  globalVaultSignerBump: number;
  /** layout version */
  version: number;
  claimedSeatCount: number;
  openOrderCount: number;
  pendingGlobalVaultAdmin: PublicKey;
};

export function decodeGlobalVaultFixed(buf: Buffer): GlobalVaultFixed {
  if (buf.length < GLOBAL_VAULT_FIXED_SIZE) {
    throw new Error(`Vault buffer too small: ${buf.length} < ${GLOBAL_VAULT_FIXED_SIZE}`);
  }
  const discriminator = readU64Le(buf, 0);
  if (discriminator !== GLOBAL_VAULT_FIXED_DISCRIMINANT) {
    throw new Error(
      `Vault discriminator mismatch: 0x${discriminator.toString(16)} != 0x${GLOBAL_VAULT_FIXED_DISCRIMINANT.toString(16)}`,
    );
  }
  return {
    discriminator,
    mint: new PublicKey(buf.subarray(8, 40)),
    globalVaultAdmin: new PublicKey(buf.subarray(40, 72)),
    integrationPool: new PublicKey(buf.subarray(72, 104)),
    integrationAccount: new PublicKey(buf.subarray(104, 136)),
    globalVaultSigner: new PublicKey(buf.subarray(136, 168)),
    lendingPool: new PublicKey(buf.subarray(168, 200)),
    riskProfilesRootIndex: buf.readUInt32LE(200),
    claimedSeatsRootIndex: buf.readUInt32LE(204),
    marketOrdersRootIndex: buf.readUInt32LE(208),
    profileFreeListHeadIndex: buf.readUInt32LE(212),
    nodeFreeListHeadIndex: buf.readUInt32LE(216),
    numBytesAllocated: buf.readUInt32LE(220),
    riskProfileCount: buf.readUInt8(224),
    globalVaultSignerBump: buf.readUInt8(225),
    version: buf.readUInt8(226),
    claimedSeatCount: buf.readUInt32LE(228),
    openOrderCount: buf.readUInt32LE(232),
    pendingGlobalVaultAdmin: new PublicKey(buf.subarray(240, 272)),
  };
}
