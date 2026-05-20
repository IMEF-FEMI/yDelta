import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, userAccountPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 1 — `ClaimSeat`. Permissionless. Auto-creates the signer's
 * `UserAccount` PDA on first call (signer pays rent).
 */
export function claimSeatInstruction(args: {
  payer: PublicKey;
  market: PublicKey;
}): TransactionInstruction {
  const data = new Writer().u8(InstructionTag.ClaimSeat).toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(args.market),
      ro(SystemProgram.programId),
      rw(userAccountPda(args.payer)[0]),
    ],
    data,
  );
}
