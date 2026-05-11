import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, userAccountPda } from '../../utils/pdas';

export const syncMarketPositionInstructionDiscriminator = YdeltaInstructionTag.SyncMarketPosition;

export type SyncMarketPositionInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  owner: PublicKey;
};

export function createSyncMarketPositionInstruction(
  accounts: SyncMarketPositionInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [ownerUserAccount] = userAccountPda(accounts.owner, programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: ownerUserAccount, isWritable: true, isSigner: false },
    { pubkey: accounts.market, isWritable: false, isSigner: false },
    { pubkey: accounts.owner, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([syncMarketPositionInstructionDiscriminator]),
  });
}
