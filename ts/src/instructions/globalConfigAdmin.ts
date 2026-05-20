/**
 * Tags 29–31 — protocol-wide admin transfers + global pause toggle. All four
 * take only `(signer, global_config)` accounts. `CreateGlobalConfig` (tag 28)
 * is in its own file because it carries the system_program + program_data.
 */
import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda } from '../pdas.js';
import { rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

export function transferProtocolAdminInstruction(args: {
  currentAdmin: PublicKey;
  newAdmin: PublicKey;
}): TransactionInstruction {
  const data = new Writer()
    .u8(InstructionTag.TransferProtocolAdmin)
    .pubkey(args.newAdmin)
    .toBuffer();
  return ydeltaIx([signerRw(args.currentAdmin), rw(globalConfigPda()[0])], data);
}

export function acceptProtocolAdminInstruction(args: {
  pendingAdmin: PublicKey;
}): TransactionInstruction {
  const data = new Writer().u8(InstructionTag.AcceptProtocolAdmin).toBuffer();
  return ydeltaIx([signerRw(args.pendingAdmin), rw(globalConfigPda()[0])], data);
}

export function setGlobalPauseInstruction(args: {
  admin: PublicKey;
  paused: boolean;
}): TransactionInstruction {
  const data = new Writer()
    .u8(InstructionTag.SetGlobalPause)
    .u8(args.paused ? 1 : 0)
    .toBuffer();
  return ydeltaIx([signerRw(args.admin), rw(globalConfigPda()[0])], data);
}
