import {
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  globalConfigPda,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 8 — `CreateVault`. One-shot per debt mint. Payer becomes the initial
 * `global_vault_admin`. Initialises the vault's marginfi integration account
 * + per-mint staging vault. The on-chain processor switches between classic
 * SPL and Token-2022 from `mint.owner`; both program ids are unconditionally
 * passed in the account list, so the caller does not need to flag the variant.
 */
export interface CreateVaultArgs {
  payer: PublicKey;
  mint: PublicKey;
  marginfiGroup: PublicKey;
  lendingPool: PublicKey;
  marginfiProgram: PublicKey;
}

export function createVaultInstruction(args: CreateVaultArgs): TransactionInstruction {
  // v1 (D1): vaults are bank-keyed; mint is still passed as a readonly account.
  const vault = globalVaultPda(args.lendingPool)[0];
  const signerPda = globalVaultSignerPda(vault)[0];
  const integration = globalVaultIntegrationAccountPda(vault)[0];
  const staging = globalVaultStagingPda(vault)[0];
  const data = new Writer().u8(InstructionTag.CreateVault).toBuffer();
  const keys = [
    signerRw(args.payer),
    ro(globalConfigPda()[0]),
    rw(vault),
    ro(args.mint),
    ro(signerPda),
    rw(integration),
    rw(staging),
    ro(TOKEN_PROGRAM_ID),
    ro(TOKEN_2022_PROGRAM_ID),
    ro(args.marginfiGroup),
    ro(args.lendingPool),
    ro(args.marginfiProgram),
    ro(SystemProgram.programId),
  ];
  return ydeltaIx(keys, data);
}
