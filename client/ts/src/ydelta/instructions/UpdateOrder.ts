import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, userAccountPda } from '../../utils/pdas';
import { UpdateOrderParams } from '../types/UpdateOrderParams';

export const updateOrderInstructionDiscriminator = YdeltaInstructionTag.UpdateOrder;

export type UpdateOrderInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
};

export type UpdateOrderInstructionArgs = UpdateOrderParams;

const UpdateOrderStruct = new beet.FixableBeetArgsStruct<
  UpdateOrderInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['orderSequenceNumber', beet.u64],
    ['orderIndexHint', beet.coption(beet.u32)],
    ['seatIndexHint', beet.coption(beet.u32)],
    ['newRateBps', beet.coption(beet.u16)],
    ['newTermSeconds', beet.coption(beet.u32)],
    ['newPrincipalAtoms', beet.coption(beet.u64)],
    ['newCollateralAtoms', beet.coption(beet.u64)],
    ['newLastValidUnixTs', beet.coption(beet.i64)],
    ['newFlags', beet.coption(beet.u8)],
    ['newAskingPriceAtoms', beet.coption(beet.u64)],
  ],
  'UpdateOrderInstructionArgs',
);

export function createUpdateOrderInstruction(
  accounts: UpdateOrderInstructionAccounts,
  args: UpdateOrderInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = UpdateOrderStruct.serialize({
    instructionDiscriminator: updateOrderInstructionDiscriminator,
    ...args,
  });
  const [userAccount] = userAccountPda(accounts.payer, programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
