import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import {
  globalConfigPda,
  globalVaultPda,
  globalVaultSignerPda,
  vaultIntegrationAccountPda,
  vaultStagingPda,
} from '../../utils/pdas';

export const createVaultInstructionDiscriminator = YdeltaInstructionTag.CreateVault;

export type CreateVaultInstructionAccounts = {
  payer: PublicKey;
  mint: PublicKey;
  marginfiGroup: PublicKey;
  lendingPool: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram: PublicKey;
  tokenProgram22: PublicKey;
};

export function createCreateVaultInstruction(
  accounts: CreateVaultInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [vault] = globalVaultPda(accounts.mint, programId);
  const [globalVaultSigner] = globalVaultSignerPda(vault, programId);
  const [integrationAccount] = vaultIntegrationAccountPda(vault, programId);
  const [globalVaultStaging] = vaultStagingPda(vault, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: accounts.mint, isWritable: false, isSigner: false },
    { pubkey: globalVaultSigner, isWritable: false, isSigner: false },
    { pubkey: integrationAccount, isWritable: true, isSigner: false },
    { pubkey: globalVaultStaging, isWritable: true, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.tokenProgram22, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: accounts.lendingPool, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([createVaultInstructionDiscriminator]),
  });
}
