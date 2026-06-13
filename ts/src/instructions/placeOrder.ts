import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  globalVaultPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
  userAccountPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 4 — `PlaceOrder`. Borrower IOC bid. Heavy instruction — callers
 * MUST prepend `withCuBudget(...)` (the bid walk + per-cross matching +
 * potential P2Pool fallback CPI is well above the default 200k CU limit).
 *
 * The `globalVault` is always passed and is derived from `debtBank` (the
 * market's debt lending pool). If the vault doesn't exist on-chain (no
 * `create_vault` yet for this bank), the loader downgrades the slot to None
 * and vault crosses are skipped — but the account meta still has to be
 * present for the AccountMeta array to line up.
 *
 * `residualMode` (v1 D6) picks the unfilled-residual path: `0` P2Pool
 * fallback (default), `1` rest the residual as a bid, `2` drop it.
 * `lastValidUnixTs` is the rested bid's expiry (`0` = never) and only
 * matters for `residualMode === 1`.
 */
export interface PlaceOrderArgs {
  payer: PublicKey;
  market: PublicKey;
  debtMint: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  borrowerDebtToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiProgram: PublicKey;
  rateBps: number;
  termSeconds: number;
  principalAtoms: bigint | BN | number;
  collateralAtoms: bigint | BN | number;
  /** Unfilled-residual path: 0 P2Pool fallback, 1 rest, 2 drop. */
  residualMode?: number;
  /** Rested-bid expiry unix ts; `0` = never (only used with residualMode 1). */
  lastValidUnixTs?: bigint | BN | number;
  seatIndexHint?: number | null;
}

export function placeOrderInstruction(args: PlaceOrderArgs): TransactionInstruction {
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const lenderMa = lenderIntegrationAccountPda(args.market)[0];
  const marketDebtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const vault = globalVaultPda(args.debtBank)[0];

  const data = new Writer()
    .u8(InstructionTag.PlaceOrder)
    .optionDataIndex(args.seatIndexHint ?? null)
    .u8(args.residualMode ?? 0)
    .i64(args.lastValidUnixTs ?? 0)
    .u16(args.rateBps)
    .u32(args.termSeconds)
    .u64(args.principalAtoms)
    .u64(args.collateralAtoms)
    .toBuffer();

  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(args.market),
      ro(SystemProgram.programId),
      ro(args.marginfiGroup),
      rw(borrowerMa),
      rw(args.debtBank),
      rw(args.collateralBank),
      ...args.debtOracles.map(ro),
      ...args.collateralOracles.map(ro),
      ro(marketSigner),
      ro(args.marginfiProgram),
      rw(args.debtLiquidityVault),
      ro(args.debtBankLiquidityVaultAuthority),
      rw(args.borrowerDebtToken),
      ro(args.tokenProgram),
      rw(userAccountPda(args.payer)[0]),
      ro(SystemProgram.programId),
      rw(lenderMa),
      rw(marketDebtVault),
      rw(vault),
    ],
    data,
  );
}
