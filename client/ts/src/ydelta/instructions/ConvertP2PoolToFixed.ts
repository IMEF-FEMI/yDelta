import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import BN from 'bn.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import {
  borrowerMarginfiAccountPda,
  globalConfigPda,
  lenderMarginfiAccountPda,
  loanPda,
  marketSignerPda,
  marketVaultPda,
} from '../../utils/pdas';

export const convertP2PoolToFixedInstructionDiscriminator = YdeltaInstructionTag.ConvertP2PoolToFixed;

const Struct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  maxAcceptableRateBps: number;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['maxAcceptableRateBps', beet.u16],
  ],
  'ConvertP2PoolToFixedInstructionArgs',
);

export type ConvertP2PoolToFixedInstructionAccounts = {
  borrower: PublicKey;
  market: PublicKey;
  loanSequence: BN | number | bigint;
  debtMint: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLva: PublicKey;
  debtOracles: PublicKey[];
  collateralBank: PublicKey;
  collateralOracles: PublicKey[];
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  crankerRefund?: PublicKey;
};

export function createConvertP2PoolToFixedInstruction(
  accounts: ConvertP2PoolToFixedInstructionAccounts,
  args: { maxAcceptableRateBps: number },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: convertP2PoolToFixedInstructionDiscriminator,
    ...args,
  });
  const seq =
    typeof accounts.loanSequence === 'bigint'
      ? accounts.loanSequence
      : BigInt(accounts.loanSequence.toString());
  const [borrowerMarginfi] = borrowerMarginfiAccountPda(accounts.market, programId);
  const [lenderMarginfi] = lenderMarginfiAccountPda(accounts.market, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);
  const [loan] = loanPda(accounts.market, seq, programId);
  const [marketDebtVault] = marketVaultPda(accounts.market, accounts.debtMint, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.borrower, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: loan, isWritable: true, isSigner: false },
    { pubkey: borrowerMarginfi, isWritable: true, isSigner: false },
    { pubkey: lenderMarginfi, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.debtLiquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBankLva, isWritable: false, isSigner: false },
  ];
  for (const o of accounts.debtOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  keys.push({ pubkey: accounts.collateralBank, isWritable: false, isSigner: false });
  for (const o of accounts.collateralOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  keys.push(
    { pubkey: marketDebtVault, isWritable: true, isSigner: false },
    { pubkey: accounts.debtMint, isWritable: false, isSigner: false },
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
  );
  if (accounts.crankerRefund) {
    keys.push({ pubkey: accounts.crankerRefund, isWritable: true, isSigner: false });
  }
  return new TransactionInstruction({ programId, keys, data });
}
