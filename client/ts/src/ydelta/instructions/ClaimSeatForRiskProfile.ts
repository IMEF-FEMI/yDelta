import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, globalVaultPda } from '../../utils/pdas';
import { ClaimSeatForRiskProfileParams } from '../types/ClaimSeatForRiskProfileParams';

export const claimSeatForRiskProfileInstructionDiscriminator = YdeltaInstructionTag.ClaimSeatForRiskProfile;

export type ClaimSeatForRiskProfileInstructionAccounts = {
  payer: PublicKey;
  mint: PublicKey;
  market: PublicKey;
};

export type ClaimSeatForRiskProfileInstructionArgs = ClaimSeatForRiskProfileParams;

const Struct = new beet.BeetArgsStruct<
  ClaimSeatForRiskProfileInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
    ['maxExposureAtoms', beet.u64],
  ],
  'ClaimSeatForRiskProfileInstructionArgs',
);

export function createClaimSeatForRiskProfileInstruction(
  accounts: ClaimSeatForRiskProfileInstructionAccounts,
  args: ClaimSeatForRiskProfileInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: claimSeatForRiskProfileInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
