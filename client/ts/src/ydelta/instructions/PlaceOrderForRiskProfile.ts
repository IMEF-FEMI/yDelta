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
import { PlaceOrderForRiskProfileParams } from '../types/PlaceOrderForRiskProfileParams';

export const placeOrderForRiskProfileInstructionDiscriminator = YdeltaInstructionTag.PlaceOrderForRiskProfile;

export type PlaceOrderForRiskProfileInstructionAccounts = {
  payer: PublicKey;
  mint: PublicKey;
  market: PublicKey;
};

export type PlaceOrderForRiskProfileInstructionArgs = PlaceOrderForRiskProfileParams;

const Struct = new beet.BeetArgsStruct<
  PlaceOrderForRiskProfileInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
    ['rateBps', beet.u16],
    ['termSeconds', beet.u32],
    ['flags', beet.u8],
  ],
  'PlaceOrderForRiskProfileInstructionArgs',
);

export function createPlaceOrderForRiskProfileInstruction(
  accounts: PlaceOrderForRiskProfileInstructionAccounts,
  args: PlaceOrderForRiskProfileInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: placeOrderForRiskProfileInstructionDiscriminator,
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
