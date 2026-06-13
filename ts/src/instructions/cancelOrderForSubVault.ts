import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signer, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 13 — `CancelOrderForSubVault`. Zero-CPI. Removes the market-side
 * `RestingOrder` + vault-side `SubVaultOrderRef`. Idempotent on missing (no
 * error if there's nothing to cancel). The vault PDA is derived from
 * `debtBank` (the market's debt lending pool).
 */
export function cancelOrderForSubVaultInstruction(args: {
  feePayer: PublicKey;
  curator: PublicKey;
  debtBank: PublicKey;
  market: PublicKey;
  subVaultId: number;
}): TransactionInstruction {
  const vault = globalVaultPda(args.debtBank)[0];
  const data = new Writer()
    .u8(InstructionTag.CancelOrderForSubVault)
    .u16(args.subVaultId)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.feePayer),
      signer(args.curator),
      ro(globalConfigPda()[0]),
      rw(vault),
      rw(args.market),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
