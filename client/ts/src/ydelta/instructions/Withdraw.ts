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
import { WithdrawParams } from '../types/WithdrawParams';

export const withdrawInstructionDiscriminator = YdeltaInstructionTag.Withdraw;

export type WithdrawInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  mint: PublicKey;
  debtMint: PublicKey;
  traderToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  liquidityVault: PublicKey;
  bankLiquidityVaultAuthority: PublicKey;
  /** Ordered oracle accounts for the debt bank (per its `OracleSetup`). */
  debtOracles: PublicKey[];
  /** Ordered oracle accounts for the collateral bank. */
  collateralOracles: PublicKey[];
  marginfiProgram: PublicKey;
};

export type WithdrawInstructionArgs = WithdrawParams;

const WithdrawStruct = new beet.FixableBeetArgsStruct<
  WithdrawInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['amountAtoms', beet.u64],
    ['traderIndexHint', beet.coption(beet.u32)],
  ],
  'WithdrawInstructionArgs',
);

export function createWithdrawInstruction(
  accounts: WithdrawInstructionAccounts,
  args: WithdrawInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = WithdrawStruct.serialize({
    instructionDiscriminator: withdrawInstructionDiscriminator,
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
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.collateralBank, isWritable: true, isSigner: false },
    { pubkey: accounts.liquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.bankLiquidityVaultAuthority, isWritable: false, isSigner: false },
  ];
  for (const o of accounts.debtOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  for (const o of accounts.collateralOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  keys.push(
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  );
  return new TransactionInstruction({ programId, keys, data });
}
