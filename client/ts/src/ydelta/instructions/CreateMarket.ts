import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
} from '@solana/spl-token';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import {
  globalConfigPda,
  marketSignerPda,
  marketVaultPda,
  lenderMarginfiAccountPda,
  borrowerMarginfiAccountPda,
} from '../../utils/pdas';

export const createMarketInstructionDiscriminator = YdeltaInstructionTag.CreateMarket;

export type CreateMarketInstructionAccounts = {
  /** Tx signer. Must equal `GlobalConfig.protocol_admin` — the loader rejects
   *  any other key with `ProtocolAdminRequired`. */
  payer: PublicKey;
  market: PublicKey;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  marginfiProgram: PublicKey;
};

export function createCreateMarketInstruction(
  accounts: CreateMarketInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [debtVault] = marketVaultPda(accounts.market, accounts.debtMint, programId);
  const [collateralVault] = marketVaultPda(accounts.market, accounts.collateralMint, programId);
  const [lenderMarginfi] = lenderMarginfiAccountPda(accounts.market, programId);
  const [borrowerMarginfi] = borrowerMarginfiAccountPda(accounts.market, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
    { pubkey: accounts.debtMint, isWritable: false, isSigner: false },
    { pubkey: accounts.collateralMint, isWritable: false, isSigner: false },
    { pubkey: debtVault, isWritable: true, isSigner: false },
    { pubkey: collateralVault, isWritable: true, isSigner: false },
    { pubkey: TOKEN_PROGRAM_ID, isWritable: false, isSigner: false },
    { pubkey: TOKEN_2022_PROGRAM_ID, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: false, isSigner: false },
    { pubkey: accounts.collateralBank, isWritable: false, isSigner: false },
    { pubkey: lenderMarginfi, isWritable: true, isSigner: false },
    { pubkey: borrowerMarginfi, isWritable: true, isSigner: false },
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
  ];

  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([createMarketInstructionDiscriminator]),
  });
}
