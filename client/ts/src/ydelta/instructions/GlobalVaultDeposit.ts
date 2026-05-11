import * as beet from '@metaplex-foundation/beet';
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
  userAccountPda,
  vaultIntegrationAccountPda,
  vaultStagingPda,
} from '../../utils/pdas';
import { GlobalVaultDepositParams } from '../types/GlobalVaultDepositParams';

export const globalVaultDepositInstructionDiscriminator = YdeltaInstructionTag.GlobalVaultDeposit;

export type GlobalVaultDepositInstructionAccounts = {
  depositor: PublicKey;
  mint: PublicKey;
  depositorToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  lendingPool: PublicKey;
  liquidityVault: PublicKey;
  marginfiProgram: PublicKey;
};

export type GlobalVaultDepositInstructionArgs = GlobalVaultDepositParams;

const Struct = new beet.BeetArgsStruct<
  GlobalVaultDepositInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['amountAtoms', beet.u64],
    ['profileId', beet.u8],
  ],
  'GlobalVaultDepositInstructionArgs',
);

export function createGlobalVaultDepositInstruction(
  accounts: GlobalVaultDepositInstructionAccounts,
  args: GlobalVaultDepositInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: globalVaultDepositInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);
  const [globalVaultSigner] = globalVaultSignerPda(vault, programId);
  const [globalVaultStaging] = vaultStagingPda(vault, programId);
  const [integrationAccount] = vaultIntegrationAccountPda(vault, programId);
  const [userAccount] = userAccountPda(accounts.depositor, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.depositor, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: accounts.mint, isWritable: false, isSigner: false },
    { pubkey: globalVaultSigner, isWritable: false, isSigner: false },
    { pubkey: globalVaultStaging, isWritable: true, isSigner: false },
    { pubkey: accounts.depositorToken, isWritable: true, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: integrationAccount, isWritable: true, isSigner: false },
    { pubkey: accounts.lendingPool, isWritable: true, isSigner: false },
    { pubkey: accounts.liquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
