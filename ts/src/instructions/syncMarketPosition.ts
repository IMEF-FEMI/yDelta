import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, userAccountPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 7 — `SyncMarketPosition`. Permissionless utility that copies the
 * canonical seat balances for `owner` onto `owner`'s `UserAccount` mirror.
 * Use after a cranker step (e.g. `process_matched_loan`) finalised state
 * without the owner signing.
 */
export function syncMarketPositionInstruction(args: {
  payer: PublicKey;
  market: PublicKey;
  owner: PublicKey;
}): TransactionInstruction {
  const data = new Writer().u8(InstructionTag.SyncMarketPosition).toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(userAccountPda(args.owner)[0]),
      ro(args.market),
      ro(args.owner),
    ],
    data,
  );
}
