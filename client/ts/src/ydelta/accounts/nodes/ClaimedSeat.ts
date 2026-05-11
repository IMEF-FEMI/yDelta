import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import { OwnerKind } from '../../types/enums';

export const CLAIMED_SEAT_SIZE = 144;
export const OWNER_KIND_USER = 0;
export const OWNER_KIND_RISK_PROFILE = 1;

export type ClaimedSeat = {
  owner: PublicKey;
  /** fp48 marginfi share quantity (u128 raw I80F48 mantissa) */
  debtWithdrawableShares: BN;
  /** fp48 marginfi share quantity (u128 raw I80F48 mantissa) */
  debtEncumberedShares: BN;
  /** fp48 marginfi share quantity (u128 raw I80F48 mantissa) */
  collateralWithdrawableShares: BN;
  /** fp48 marginfi share quantity (u128 raw I80F48 mantissa) */
  collateralEncumberedShares: BN;
  openBorrowCount: number;
  openLendCount: number;
  ownerKind: OwnerKind;
  riskProfileId: number;
  /** bps */
  riskProfileMaxLtvBps: number;
};

function readU128(buf: Buffer, offset: number): BN {
  return new BN(buf.subarray(offset, offset + 16), 'le');
}

export function decodeClaimedSeat(buf: Buffer): ClaimedSeat {
  return {
    owner: new PublicKey(buf.subarray(0, 32)),
    debtWithdrawableShares: readU128(buf, 32),
    debtEncumberedShares: readU128(buf, 48),
    collateralWithdrawableShares: readU128(buf, 64),
    collateralEncumberedShares: readU128(buf, 80),
    openBorrowCount: buf.readUInt32LE(96),
    openLendCount: buf.readUInt32LE(100),
    ownerKind: buf.readUInt8(104) as OwnerKind,
    riskProfileId: buf.readUInt8(105),
    riskProfileMaxLtvBps: buf.readUInt16LE(106),
  };
}

/** For a vault-owned seat, the `collateral_*_shares` u128 slots are repurposed
 *  to carry the per-(profile, market) cap data — `max_exposure_atoms` and
 *  `deployed_atoms`. The on-chain Rust code asserts `ownerKind === Vault` before
 *  calling these; do the same on the TS side. */
export function vaultSeatMaxExposureAtoms(seat: ClaimedSeat): BN {
  if (seat.ownerKind !== OwnerKind.Vault) {
    throw new Error('max_exposure_atoms only meaningful on vault seats');
  }
  // u64 stored in low 8 bytes of the u128 slot.
  return seat.collateralWithdrawableShares.maskn(64);
}

export function vaultSeatDeployedAtoms(seat: ClaimedSeat): BN {
  if (seat.ownerKind !== OwnerKind.Vault) {
    throw new Error('deployed_atoms only meaningful on vault seats');
  }
  return seat.collateralEncumberedShares.maskn(64);
}
