import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 36 — toggle `GlobalVaultFixed.is_paused`. Vault-admin-gated. The two-step
 * admin-transfer ixs stay live while paused so a stuck vault can still rotate
 * its admin for recovery.
 */
export function setVaultPauseInstruction(args: {
  admin: PublicKey;
  bank: PublicKey;
  paused: boolean;
}): TransactionInstruction {
  const vault = globalVaultPda(args.bank)[0];
  const data = new Writer()
    .u8(InstructionTag.SetVaultPause)
    .u8(args.paused ? 1 : 0)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.admin), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
