import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
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
import { ClaimCuratorFeeParams } from '../types/ClaimCuratorFeeParams';

export const claimCuratorFeeInstructionDiscriminator = YdeltaInstructionTag.ClaimCuratorFee;

export type ClaimCuratorFeeInstructionAccounts = {
  payer: PublicKey;
  mint: PublicKey;
  curatorToken: PublicKey;
  debtBank: PublicKey;
  liquidityVault: PublicKey;
  bankLiquidityVaultAuthority: PublicKey;
  bankOracle: PublicKey;
  tokenProgram: PublicKey;
  marginfiProgram: PublicKey;
  marginfiGroup: PublicKey;
};

export type ClaimCuratorFeeInstructionArgs = ClaimCuratorFeeParams;

const Struct = new beet.BeetArgsStruct<
  ClaimCuratorFeeInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
  ],
  'ClaimCuratorFeeInstructionArgs',
);

export function createClaimCuratorFeeInstruction(
  accounts: ClaimCuratorFeeInstructionAccounts,
  args: ClaimCuratorFeeInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: claimCuratorFeeInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);
  const [globalVaultSigner] = globalVaultSignerPda(vault, programId);
  const [globalVaultStaging] = vaultStagingPda(vault, programId);
  const [vaultIntegration] = vaultIntegrationAccountPda(vault, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: globalVaultSigner, isWritable: false, isSigner: false },
    { pubkey: globalVaultStaging, isWritable: true, isSigner: false },
    { pubkey: vaultIntegration, isWritable: true, isSigner: false },
    { pubkey: accounts.curatorToken, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.liquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.bankLiquidityVaultAuthority, isWritable: false, isSigner: false },
    { pubkey: accounts.bankOracle, isWritable: false, isSigner: false },
    { pubkey: accounts.mint, isWritable: false, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
