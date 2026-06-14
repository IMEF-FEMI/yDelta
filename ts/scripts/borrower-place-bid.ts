/**
 * borrower-place-bid.ts — `PlaceOrder` (tag 4). Borrower IOC bid that
 * crosses resting vault asks.
 *
 * Reads:
 *   .local/borrower-place-bid-input.json {
 *     marketLabel: string,
 *     borrowerKeypairPath?: string,         // path; defaults to YDELTA_KEYPAIR_PATH signer
 *     borrowerDebtTokenAta: string,         // borrower's USDC ATA
 *     rateBps: number,
 *     termSeconds: number,
 *     principalAtoms: string | number,      // u64 (string for >2^53)
 *     collateralAtoms: string | number,
 *     residualMode?: number,                // 0 P2PoolFallback, 1 rest, 2 drop
 *     ltvBufferBps?: number,                // optional borrower LTV buffer; tightens origination cap
 *     lastValidUnixTs?: string | number,    // i64; rested-bid expiry (0 = never)
 *     seatIndexHint?: number | null
 *   }
 *
 * The market loaders read both debt + collateral oracles, so we crank
 * both banks here before sending. Wraps the ix with the heavy-CU
 * preamble (`withCuBudget`).
 */
import { ComputeBudgetProgram, PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  claimSeatIfNeededInstructions,
  placeOrderInstruction,
} from '../src/instructions/index.js';
import { OracleSetup } from '../src/marginfi.js';
import {
  appendTxLog,
  ataFor,
  createAtaIdempotentIx,
  loadBorrower,
  loadConnection,
  log,
  readJson,
  sendV0Ixs,
} from './_runner.js';
import {
  MARGINFI_PROGRAM_ID,
  readBankOnchain,
} from './_marginfi.js';
import { buildSwitchboardUpdateIxs, crankStaleBankOracles } from './_oracleCrank.js';
import type { MarketDump } from './_types.js';

const isPyth = (s: OracleSetup) =>
  s === OracleSetup.PythPushOracle || s === OracleSetup.StakedWithPythPush;
const isSwitchboard = (s: OracleSetup) => s === OracleSetup.SwitchboardPull;

interface Input {
  marketLabel: string;
  rateBps: number;
  termSeconds: number;
  principalAtoms: string | number;
  collateralAtoms: string | number;
  /** Unfilled-residual path: 0 P2Pool fallback, 1 rest, 2 drop. */
  residualMode?: number;
  /** Optional borrower LTV buffer in bps (default 0): originate strictly
   *  below the sub-vault cap (effective_cap = max_ltv - buffer). */
  ltvBufferBps?: number;
  /** Rested-bid expiry unix ts; `0` = never (only used with residualMode 1). */
  lastValidUnixTs?: string | number;
  seatIndexHint?: number | null;
}

async function main(): Promise<void> {
  const input = readJson<Input>('borrower-place-bid-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const borrower = loadBorrower();

  const marketPk = new PublicKey(market.market);
  const debtMint = new PublicKey(market.debtMint);
  const debtBank = new PublicKey(market.debtBank);
  const collateralBank = new PublicKey(market.collateralBank);
  const debtLiquidityVault = new PublicKey(market.debtLiquidityVault);
  const debtBankLiquidityVaultAuthority = new PublicKey(
    market.debtBankLiquidityVaultAuthority,
  );
  const debtOracles = market.debtOracles.map((s) => new PublicKey(s));
  const collateralOracles = market.collateralOracles.map((s) => new PublicKey(s));

  // We re-read bank state every run so oracle staleness reflects "now".
  const debtBankState = await readBankOnchain(conn, debtBank);
  const collateralBankState = await readBankOnchain(conn, collateralBank);

  // Pyth feeds: crank conditionally in a separate upfront tx (skip when
  // fresh; a Pyth post is multi-tx so it can't be bundled).
  const crank = await crankStaleBankOracles(
    conn,
    borrower,
    [
      { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
      { bank: collateralBankState, pythFeedIdHex: market.collateralPythFeedIdHex, pythShardId: market.collateralPythShardId },
    ].filter((b) => isPyth(b.bank.oracleSetup)),
  );

  // Switchboard feeds: always build an update ix and bundle it INTO the
  // place-order tx so the fresh price and the LTV cross execute atomically.
  const sbOracleKeys: PublicKey[] = [];
  if (isSwitchboard(debtBankState.oracleSetup)) sbOracleKeys.push(debtBankState.oracleKeys[0]);
  if (isSwitchboard(collateralBankState.oracleSetup)) sbOracleKeys.push(collateralBankState.oracleKeys[0]);
  const { ixs: sbIxs, luts: sbLuts } = await buildSwitchboardUpdateIxs(conn, borrower, sbOracleKeys);

  // The market LUT compresses place_order's account list so the bundle fits
  // the 1232-byte tx limit.
  if (!market.lookupTable) {
    throw new Error(
      `markets.json[${market.label}] has no lookupTable — run \`yarn create-market-lut\` first`,
    );
  }
  const lutRes = await conn.getAddressLookupTable(new PublicKey(market.lookupTable));
  if (!lutRes.value) {
    throw new Error(`market LUT ${market.lookupTable} not found on-chain (not yet active?)`);
  }

  const seatIxs = await claimSeatIfNeededInstructions({
    market: marketPk,
    payer: borrower.publicKey,
    connection: conn,
  });

  const principal = BigInt(input.principalAtoms.toString());
  const collateral = BigInt(input.collateralAtoms.toString());

  log(`[borrower-place-bid] market=${marketPk.toBase58()} principal=${principal} collateral=${collateral} borrower=${borrower.publicKey.toBase58()}`);
  log(`[borrower-place-bid] rate=${input.rateBps}bps term=${input.termSeconds}s residualMode=${input.residualMode ?? 0} lastValidUnixTs=${input.lastValidUnixTs ?? 0}`);

  const ix = placeOrderInstruction({
    payer: borrower.publicKey,
    market: marketPk,
    debtMint,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    debtBank,
    collateralBank,
    debtOracles,
    collateralOracles,
    debtLiquidityVault,
    debtBankLiquidityVaultAuthority,
    borrowerDebtToken: ataFor(borrower.publicKey, debtMint),
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiProgram: MARGINFI_PROGRAM_ID,
    rateBps: input.rateBps,
    termSeconds: input.termSeconds,
    principalAtoms: principal,
    collateralAtoms: collateral,
    residualMode: input.residualMode,
    ltvBufferBps: input.ltvBufferBps,
    lastValidUnixTs: input.lastValidUnixTs === undefined
      ? undefined
      : BigInt(input.lastValidUnixTs.toString()),
    seatIndexHint: input.seatIndexHint ?? null,
  });

  // Switchboard ixs MUST lead the tx (their precompile references data by
  // absolute instruction index). Order: SB, CU, claim-seat, ensure-ATA, bid.
  const cuIxs = [
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }),
    ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }),
  ];
  const sig = await sendV0Ixs(
    conn,
    borrower,
    [
      ...sbIxs,
      ...cuIxs,
      ...seatIxs,
      createAtaIdempotentIx(borrower.publicKey, borrower.publicKey, debtMint),
      ix,
    ],
    { luts: [lutRes.value, ...sbLuts] },
  );
  log(`[borrower-place-bid] signature = ${sig} (bundled ${sbIxs.length} switchboard ix)`);
  appendTxLog({
    script: 'borrower-place-bid',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      borrower: borrower.publicKey.toBase58(),
      rateBps: input.rateBps,
      termSeconds: input.termSeconds,
      principalAtoms: input.principalAtoms.toString(),
      collateralAtoms: input.collateralAtoms.toString(),
      residualMode: input.residualMode ?? 0,
      lastValidUnixTs: (input.lastValidUnixTs ?? 0).toString(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
