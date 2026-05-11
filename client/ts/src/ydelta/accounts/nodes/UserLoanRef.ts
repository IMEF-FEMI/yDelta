import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import { UserLoanRole, CounterpartyKind } from '../../types/enums';

export const USER_LOAN_REF_SIZE = 144;

export type UserLoanRef = {
  loan: PublicKey;
  market: PublicKey;
  principalAtoms: BN;
  startedAtUnix: BN;
  maturesAtUnix: BN;
  rateBps: number;
  role: UserLoanRole;
  counterpartyKind: CounterpartyKind;
  counterpartyProfileId: number;
};

function readI64(buf: Buffer, offset: number): BN {
  const u = new BN(buf.subarray(offset, offset + 8), 'le');
  const TWO_64 = new BN(1).shln(64);
  return u.testn(63) ? u.sub(TWO_64) : u;
}

export function decodeUserLoanRef(buf: Buffer): UserLoanRef {
  return {
    loan: new PublicKey(buf.subarray(0, 32)),
    market: new PublicKey(buf.subarray(32, 64)),
    principalAtoms: new BN(buf.subarray(64, 72), 'le'),
    startedAtUnix: readI64(buf, 72),
    maturesAtUnix: readI64(buf, 80),
    rateBps: buf.readUInt16LE(88),
    role: buf.readUInt8(90) as UserLoanRole,
    counterpartyKind: buf.readUInt8(91) as CounterpartyKind,
    counterpartyProfileId: buf.readUInt8(92),
  };
}
