import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda } from '../../utils/pdas';
import { SetFeeConfigParams } from '../types/SetFeeConfigParams';

export const setFeeConfigInstructionDiscriminator = YdeltaInstructionTag.SetFeeConfig;

export type SetFeeConfigInstructionAccounts = {
  admin: PublicKey;
  market: PublicKey;
};

export type SetFeeConfigInstructionArgs = SetFeeConfigParams;

const Struct = new beet.FixableBeetArgsStruct<
  SetFeeConfigInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['protocolFeeBpsFloor', beet.coption(beet.u16)],
    ['originationBps', beet.coption(beet.u16)],
    ['curatorSplitBps', beet.coption(beet.u16)],
    ['curatorFeeBps', beet.coption(beet.u16)],
    ['liquidationKeeperBps', beet.coption(beet.u16)],
    ['liquidationProtocolBps', beet.coption(beet.u16)],
    ['ltvBufferBps', beet.coption(beet.u16)],
    ['gracePeriodSeconds', beet.coption(beet.u32)],
  ],
  'SetFeeConfigInstructionArgs',
);

export function createSetFeeConfigInstruction(
  accounts: SetFeeConfigInstructionAccounts,
  args: SetFeeConfigInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: setFeeConfigInstructionDiscriminator,
    ...args,
  });
  const keys: AccountMeta[] = [
    { pubkey: accounts.admin, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
