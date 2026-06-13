import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 18 — `SetFeeConfig`. Each field is independent: pass `null` /
 * `undefined` to leave unchanged, or a number to update. Runs even while
 * the market is paused — it's a pure admin-only header mutation with no
 * atom flow, useful for retuning fees on a halted market before resuming.
 */
export interface SetFeeConfigArgs {
  admin: PublicKey;
  market: PublicKey;
  protocolFeeBpsFloor?: number | null;
  originationBps?: number | null;
  curatorSplitBps?: number | null;
  liquidationKeeperBps?: number | null;
  liquidationProtocolBps?: number | null;
  gracePeriodSeconds?: number | null;
}

export function setFeeConfigInstruction(args: SetFeeConfigArgs): TransactionInstruction {
  const data = new Writer()
    .u8(InstructionTag.SetFeeConfig)
    .optionU16(args.protocolFeeBpsFloor ?? null)
    .optionU16(args.originationBps ?? null)
    .optionU16(args.curatorSplitBps ?? null)
    .optionU16(args.liquidationKeeperBps ?? null)
    .optionU16(args.liquidationProtocolBps ?? null)
    .optionU32(args.gracePeriodSeconds ?? null)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.admin),
      ro(globalConfigPda()[0]),
      rw(args.market),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
