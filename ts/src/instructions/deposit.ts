import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
  userAccountPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 2 — `Deposit`. Side (debt vs collateral) is disambiguated by `mint`:
 * if `mint == debtMint`, atoms hop into the lender-side marginfi account;
 * else into the borrower-side account.
 *
 * Caller must supply the correct `bank` + `liquidityVault` for the chosen
 * mint. `tokenProgram` defaults to SPL Token classic — pass the Token-2022
 * program id explicitly for 2022 mints.
 */
export interface DepositArgs {
  payer: PublicKey;
  market: PublicKey;
  /** Mint of the side being deposited. */
  mint: PublicKey;
  /** Debt mint for the market — used to pick lender vs borrower marginfi account. */
  debtMint: PublicKey;
  traderToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  bank: PublicKey;
  liquidityVault: PublicKey;
  marginfiProgram: PublicKey;
  amountAtoms: bigint | BN | number;
  /** Hypertree seat-index hint; pass `null` to skip the tree walk. */
  traderIndexHint?: number | null;
}

export function depositInstruction(args: DepositArgs): TransactionInstruction {
  const isDebt = args.mint.equals(args.debtMint);
  const marginfiAccount = isDebt
    ? lenderIntegrationAccountPda(args.market)[0]
    : borrowerIntegrationAccountPda(args.market)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const vault = marketTokenVaultPda(args.market, args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.Deposit)
    .u64(args.amountAtoms)
    .optionDataIndex(args.traderIndexHint ?? null)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(args.market),
      rw(args.traderToken),
      rw(vault),
      ro(args.tokenProgram),
      ro(args.mint),
      ro(args.marginfiGroup),
      rw(marginfiAccount),
      rw(args.bank),
      rw(args.liquidityVault),
      ro(marketSigner),
      ro(args.marginfiProgram),
      rw(userAccountPda(args.payer)[0]),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
