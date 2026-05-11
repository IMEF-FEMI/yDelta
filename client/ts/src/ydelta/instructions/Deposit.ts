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
  borrowerMarginfiAccountPda,
  globalConfigPda,
  lenderMarginfiAccountPda,
  marketSignerPda,
  marketVaultPda,
  userAccountPda,
} from '../../utils/pdas';
import { DepositParams } from '../types/DepositParams';

export const depositInstructionDiscriminator = YdeltaInstructionTag.Deposit;

export type DepositInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  mint: PublicKey;
  debtMint: PublicKey;
  traderToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  bank: PublicKey;
  liquidityVault: PublicKey;
  marginfiProgram: PublicKey;
};

export type DepositInstructionArgs = DepositParams;

const DepositStruct = new beet.FixableBeetArgsStruct<
  DepositInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['amountAtoms', beet.u64],
    ['traderIndexHint', beet.coption(beet.u32)],
  ],
  'DepositInstructionArgs',
);

export function createDepositInstruction(
  accounts: DepositInstructionAccounts,
  args: DepositInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = DepositStruct.serialize({
    instructionDiscriminator: depositInstructionDiscriminator,
    ...args,
  });
  const isDebt = accounts.mint.equals(accounts.debtMint);
  const [marginfiAccount] = isDebt
    ? lenderMarginfiAccountPda(accounts.market, programId)
    : borrowerMarginfiAccountPda(accounts.market, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);
  const [vault] = marketVaultPda(accounts.market, accounts.mint, programId);
  const [userAccount] = userAccountPda(accounts.payer, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: accounts.traderToken, isWritable: true, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.mint, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: marginfiAccount, isWritable: true, isSigner: false },
    { pubkey: accounts.bank, isWritable: true, isSigner: false },
    { pubkey: accounts.liquidityVault, isWritable: true, isSigner: false },
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];

  return new TransactionInstruction({ programId, keys, data });
}
