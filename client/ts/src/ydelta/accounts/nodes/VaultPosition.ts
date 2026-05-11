import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';

export const VAULT_POSITION_SIZE = 144;

/** User-side mirror of a RiskProfileDepositorSeat — same wire shape, kept by UserAccount for off-chain rollups. */
export type VaultPosition = {
  vault: PublicKey;
  profileId: number;
  /** fp48 share units */
  shares: BN;
  /** Aave-style cumulative yield index, × 2^48 */
  snapshotSupplyYieldIndexScaled: BN;
  /** Aave-style cumulative yield index, × 2^48 */
  snapshotDeltaYieldIndexScaled: BN;
  /** unix seconds */
  lastUpdatedUnix: BN;
};

function readU128(buf: Buffer, offset: number): BN {
  return new BN(buf.subarray(offset, offset + 16), 'le');
}

function readI64(buf: Buffer, offset: number): BN {
  const u = new BN(buf.subarray(offset, offset + 8), 'le');
  const TWO_64 = new BN(1).shln(64);
  return u.testn(63) ? u.sub(TWO_64) : u;
}

export function decodeVaultPosition(buf: Buffer): VaultPosition {
  return {
    vault: new PublicKey(buf.subarray(0, 32)),
    profileId: buf.readUInt8(32),
    shares: readU128(buf, 48),
    snapshotSupplyYieldIndexScaled: readU128(buf, 64),
    snapshotDeltaYieldIndexScaled: readU128(buf, 80),
    lastUpdatedUnix: readI64(buf, 96),
  };
}
