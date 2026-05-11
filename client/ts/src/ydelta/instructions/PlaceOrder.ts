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
  globalVaultPda,
  lenderMarginfiAccountPda,
  marketSignerPda,
  marketVaultPda,
  userAccountPda,
} from '../../utils/pdas';
import { PlaceOrderParams } from '../types/PlaceOrderParams';

export const placeOrderInstructionDiscriminator = YdeltaInstructionTag.PlaceOrder;

export type PlaceOrderInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  borrowerDebtToken: PublicKey;
  debtMint: PublicKey;
  tokenProgram: PublicKey;
  marginfiProgram: PublicKey;
  /** Append only when placing a `SecondaryLoanSale` bid. */
  secondaryLoan?: PublicKey;
};

export type PlaceOrderInstructionArgs = PlaceOrderParams;

const PlaceOrderStruct = new beet.FixableBeetArgsStruct<
  PlaceOrderInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['seatIndexHint', beet.coption(beet.u32)],
    ['side', beet.u8],
    ['orderType', beet.u8],
    ['flags', beet.u8],
    ['kind', beet.u8],
    ['rateBps', beet.u16],
    ['termSeconds', beet.u32],
    ['principalAtoms', beet.u64],
    ['collateralAtoms', beet.u64],
    ['askingPriceAtoms', beet.u64],
    ['lastValidUnixTs', beet.i64],
    ['borrowerLtvBps', beet.coption(beet.u16)],
  ],
  'PlaceOrderInstructionArgs',
);

export function createPlaceOrderInstruction(
  accounts: PlaceOrderInstructionAccounts,
  args: PlaceOrderInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = PlaceOrderStruct.serialize({
    instructionDiscriminator: placeOrderInstructionDiscriminator,
    ...args,
  });
  const [marginfiAccount] = borrowerMarginfiAccountPda(accounts.market, programId);
  const [lenderMarginfi] = lenderMarginfiAccountPda(accounts.market, programId);
  const [marketDebtVault] = marketVaultPda(accounts.market, accounts.debtMint, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);
  const [userAccount] = userAccountPda(accounts.payer, programId);
  const [vault] = globalVaultPda(accounts.debtMint, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: marginfiAccount, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.collateralBank, isWritable: true, isSigner: false },
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
    { pubkey: accounts.debtLiquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBankLiquidityVaultAuthority, isWritable: false, isSigner: false },
    { pubkey: accounts.borrowerDebtToken, isWritable: true, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
    { pubkey: lenderMarginfi, isWritable: true, isSigner: false },
    { pubkey: marketDebtVault, isWritable: true, isSigner: false },
  );
  if (accounts.secondaryLoan) {
    keys.push({ pubkey: accounts.secondaryLoan, isWritable: true, isSigner: false });
  }
  keys.push({ pubkey: vault, isWritable: true, isSigner: false });

  return new TransactionInstruction({ programId, keys, data });
}
