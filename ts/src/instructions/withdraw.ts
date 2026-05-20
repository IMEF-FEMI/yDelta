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
 * Tag 3 — `Withdraw`. `withdraw_all=true` drains the seat's full withdrawable
 * balance on the selected side and ignores `amountAtoms`.
 *
 * marginfi.withdraw runs a health check, so BOTH debt and collateral oracles
 * must be passed (one of them is the side being withdrawn; the other backs
 * the health computation). Order: debtOracles first, then collateralOracles.
 */
export interface WithdrawArgs {
  payer: PublicKey;
  market: PublicKey;
  mint: PublicKey;
  debtMint: PublicKey;
  traderToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  /** marginfi liquidity vault for the side being withdrawn. */
  liquidityVault: PublicKey;
  /** Authority over the same liquidity vault. */
  bankLiquidityVaultAuthority: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  marginfiProgram: PublicKey;
  amountAtoms: bigint | BN | number;
  traderIndexHint?: number | null;
  withdrawAll?: boolean;
}

export function withdrawInstruction(args: WithdrawArgs): TransactionInstruction {
  const isDebt = args.mint.equals(args.debtMint);
  const marginfiAccount = isDebt
    ? lenderIntegrationAccountPda(args.market)[0]
    : borrowerIntegrationAccountPda(args.market)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const vault = marketTokenVaultPda(args.market, args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.Withdraw)
    .u64(args.amountAtoms)
    .optionDataIndex(args.traderIndexHint ?? null)
    .bool(args.withdrawAll ?? false)
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
      rw(args.debtBank),
      rw(args.collateralBank),
      rw(args.liquidityVault),
      ro(args.bankLiquidityVaultAuthority),
      ...args.debtOracles.map(ro),
      ...args.collateralOracles.map(ro),
      ro(marketSigner),
      ro(args.marginfiProgram),
      rw(userAccountPda(args.payer)[0]),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
