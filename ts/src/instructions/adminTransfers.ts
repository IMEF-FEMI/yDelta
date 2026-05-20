/**
 * Six admin-transfer instruction builders (tags 21..=26 + 29/30 for the
 * protocol admin, plus market/vault pause-toggles). Each one is small and
 * shares the same shape: signer is the current/pending admin, the global
 * config is read-only, and the target state account is the one mutated.
 */
import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/* ── Market admin ──────────────────────────────────────────────── */

export function transferMarketAdminInstruction(args: {
  market: PublicKey;
  currentAdmin: PublicKey;
  newAdmin: PublicKey;
}): TransactionInstruction {
  const data = new Writer()
    .u8(InstructionTag.TransferMarketAdmin)
    .pubkey(args.newAdmin)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.currentAdmin), ro(globalConfigPda()[0]), rw(args.market)],
    data,
  );
}

export function acceptMarketAdminInstruction(args: {
  market: PublicKey;
  pendingAdmin: PublicKey;
}): TransactionInstruction {
  const data = new Writer().u8(InstructionTag.AcceptMarketAdmin).toBuffer();
  return ydeltaIx(
    [signerRw(args.pendingAdmin), ro(globalConfigPda()[0]), rw(args.market)],
    data,
  );
}

/* ── Global-vault admin ────────────────────────────────────────── */

export function transferGlobalVaultAdminInstruction(args: {
  mint: PublicKey;
  currentAdmin: PublicKey;
  newAdmin: PublicKey;
}): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.TransferGlobalVaultAdmin)
    .pubkey(args.newAdmin)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.currentAdmin), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}

export function acceptGlobalVaultAdminInstruction(args: {
  mint: PublicKey;
  pendingAdmin: PublicKey;
}): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer().u8(InstructionTag.AcceptGlobalVaultAdmin).toBuffer();
  return ydeltaIx(
    [signerRw(args.pendingAdmin), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}

/* ── Curator (per-profile) ─────────────────────────────────────── */

export function transferCuratorInstruction(args: {
  mint: PublicKey;
  currentCurator: PublicKey;
  profileId: number;
  newCurator: PublicKey;
}): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.TransferCurator)
    .u8(args.profileId)
    .pubkey(args.newCurator)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.currentCurator), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}

export function acceptCuratorInstruction(args: {
  mint: PublicKey;
  pendingCurator: PublicKey;
  profileId: number;
}): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.AcceptCurator)
    .u8(args.profileId)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.pendingCurator), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
