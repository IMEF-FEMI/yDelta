import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda } from '../../utils/pdas';

export const setMarketPauseInstructionDiscriminator = YdeltaInstructionTag.SetMarketPause;

const Struct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  paused: number;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['paused', beet.u8],
  ],
  'SetMarketPauseInstructionArgs',
);

export type SetMarketPauseInstructionAccounts = {
  admin: PublicKey;
  market: PublicKey;
};

export function createSetMarketPauseInstruction(
  accounts: SetMarketPauseInstructionAccounts,
  args: { paused: boolean },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: setMarketPauseInstructionDiscriminator,
    paused: args.paused ? 1 : 0,
  });
  const keys: AccountMeta[] = [
    { pubkey: accounts.admin, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
