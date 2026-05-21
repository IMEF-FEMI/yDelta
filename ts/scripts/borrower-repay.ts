/**
 * borrower-repay.ts — `Repay` (tag 6). Supports partial repay, exact full
 * repay, and "repay all outstanding".
 *
 * Reads:
 *   .local/borrower-repay-input.json {
 *     marketLabel: string,
 *     borrowerKeypairPath?: string,
 *     borrowerDebtTokenAta: string,
 *     loanSequence: string | number,        // loan PDA seq (u64)
 *     crankerRefund: string,                // who paid the loan rent
 *     repayAtoms?: string | number,         // required unless fullRepay / repayAll
 *     fullRepay?: boolean,                  // exact close for Fixed or P2Pool
 *     repayAll?: boolean,                   // alias for fullRepay
 *     borrowerSeatIndexHint?: number | null
 *   }
 *
 * Repay doesn't touch oracles, so no crank ixs needed.
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import { repayInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  ataFor,
  createAtaIdempotentIx,
  loadBorrower,
  loadConnection,
  log,
  readJson,
  sendIxs,
} from './_runner.js';
import { MARGINFI_PROGRAM_ID } from './_marginfi.js';
import type { MarketDump } from './_types.js';

interface Input {
  marketLabel: string;
  loanSequence: string | number;
  crankerRefund: string;
  repayAtoms?: string | number;
  fullRepay?: boolean;
  repayAll?: boolean;
  borrowerSeatIndexHint?: number | null;
}

async function main(): Promise<void> {
  const input = readJson<Input>('borrower-repay-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const borrower = loadBorrower();
  const debtMint = new PublicKey(market.debtMint);

  const fullRepay = (input.fullRepay ?? false) || (input.repayAll ?? false);
  const repayAtoms = fullRepay ? 0n : BigInt(requireRepayAtoms(input.repayAtoms));
  const sequence = BigInt(input.loanSequence.toString());

  log(`[borrower-repay] market=${market.market} seq=${sequence} repay=${repayAtoms} full=${fullRepay} borrower=${borrower.publicKey.toBase58()}`);

  const ix = repayInstruction({
    borrower: borrower.publicKey,
    market: new PublicKey(market.market),
    sequence,
    debtMint,
    borrowerToken: ataFor(borrower.publicKey, debtMint),
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    debtBank: new PublicKey(market.debtBank),
    debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
    collateralBank: new PublicKey(market.collateralBank),
    marginfiProgram: MARGINFI_PROGRAM_ID,
    repayAtoms,
    fullRepay,
    borrowerSeatIndexHint: input.borrowerSeatIndexHint ?? null,
    crankerRefund: new PublicKey(input.crankerRefund),
  });
  // Source ATA must hold the repay atoms; idempotent-create is a no-op if so.
  const sig = await sendIxs(conn, borrower, [
    createAtaIdempotentIx(borrower.publicKey, borrower.publicKey, debtMint),
    ix,
  ]);
  log(`[borrower-repay] signature = ${sig}`);
  appendTxLog({
    script: 'borrower-repay',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      sequence: sequence.toString(),
      repayAtoms: repayAtoms.toString(),
      fullRepay,
      borrower: borrower.publicKey.toBase58(),
    },
  });
}

function requireRepayAtoms(v: string | number | undefined): string {
  if (v === undefined) {
    throw new Error(
      'borrower-repay-input.json: repayAtoms is required unless fullRepay/repayAll is true',
    );
  }
  return v.toString();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
