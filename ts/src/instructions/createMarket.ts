import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 0 — `CreateMarket`. Signer becomes `MarketFixed.admin` AND pays both
 * marginfi-account rents via CPI in the processor.
 *
 * The market account itself must be allocated by a separate
 * `SystemProgram.createAccount` ix at `MARKET_FIXED_SIZE = 512` bytes, owned
 * by the yDelta program. Callers typically bundle the two ixs in one tx.
 */
export interface CreateMarketArgs {
  marketCreator: PublicKey;
  market: PublicKey;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  marginfiProgram: PublicKey;
}

export function createMarketInstruction(args: CreateMarketArgs): TransactionInstruction {
  const debtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const collateralVault = marketTokenVaultPda(args.market, args.collateralMint)[0];
  const lenderMa = lenderIntegrationAccountPda(args.market)[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const data = new Writer().u8(InstructionTag.CreateMarket).toBuffer();
  return ydeltaIx(
    [
      signerRw(args.marketCreator),
      ro(globalConfigPda()[0]),
      rw(args.market),
      ro(SystemProgram.programId),
      ro(args.debtMint),
      ro(args.collateralMint),
      rw(debtVault),
      rw(collateralVault),
      ro(TOKEN_PROGRAM_ID),
      ro(TOKEN_2022_PROGRAM_ID),
      ro(args.marginfiGroup),
      ro(args.debtBank),
      ro(args.collateralBank),
      rw(lenderMa),
      rw(borrowerMa),
      ro(marketSigner),
      ro(args.marginfiProgram),
    ],
    data,
  );
}
