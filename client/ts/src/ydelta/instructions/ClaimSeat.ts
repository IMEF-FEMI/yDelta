import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, userAccountPda } from '../../utils/pdas';

export const claimSeatInstructionDiscriminator = YdeltaInstructionTag.ClaimSeat;

export type ClaimSeatInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
};

export function createClaimSeatInstruction(
  accounts: ClaimSeatInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [userAccount] = userAccountPda(accounts.payer, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
  ];

  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([claimSeatInstructionDiscriminator]),
  });
}
