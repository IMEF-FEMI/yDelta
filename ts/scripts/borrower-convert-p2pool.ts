/**
 * borrower-convert-p2pool.ts — `ConvertP2PoolToFixed` (tag 33). Borrower-
 * initiated refinance: walks the asks tree and crosses every compatible
 * vault ask whose `rate_bps ≤ maxAcceptableRateBps` AND `term_seconds ≥
 * remaining_term`. Each cross emits a fresh Fixed `MatchedLoan` queue
 * node; unfilled residual stays on the P2Pool loan body.
 *
 * Reads:
 *   .local/borrower-convert-p2pool-input.json {
 *     marketLabel: string,
 *     loanSequence: string | number,        // loan PDA seq (u64)
 *     crankerRefund: string,                // who paid the loan rent
 *     maxAcceptableRateBps: number,         // cap; matcher breaks past this
 *     ltvBufferBps?: number,                // optional borrower LTV buffer on converted loans
 *     simulateOnly?: boolean                // skip send, just print logs
 *   }
 *
 * Convert touches both bank oracles for the per-cross LTV check, so we
 * bundle a fresh Switchboard pull-feed update in front of the convert ix
 * (same pattern as borrower-place-bid). The market LUT compresses the
 * convert account list under the 1232-byte tx limit.
 *
 * On failure: dumps full preflight logs (the wallet-adapter path in the
 * UI truncates these to just the ComputeBudget invoke), so this is the
 * tool to use when you need to see WHY a refinance reverted.
 */
import {
  AddressLookupTableAccount,
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  SendTransactionError,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import { convertP2poolToFixedInstruction } from '../src/instructions/index.js';
import {
  decodeLoanFixed,
  decodeMarket,
  LOAN_FIXED_DISCRIMINANT,
  LOAN_FIXED_SIZE,
  LoanType,
  YDELTA_PROGRAM_ID,
} from '../src/index.js';
import { OracleSetup } from '../src/marginfi.js';
import bs58 from 'bs58';
import {
  appendTxLog,
  assertSendAllowed,
  loadBorrower,
  loadConnection,
  log,
  readJson,
} from './_runner.js';
import {
  MARGINFI_PROGRAM_ID,
  bankLiquidityVaultAuthority,
  bankOracleAccounts,
  readBankOnchain,
} from './_marginfi.js';
import { buildSwitchboardUpdateIxs } from './_oracleCrank.js';
import type { MarketDump } from './_types.js';

interface Input {
  marketLabel: string;
  /** `"list"` triggers discovery mode — script prints every P2Pool loan
   *  the connected borrower owns on this market (with seq + crankerRefund)
   *  and exits without sending. */
  loanSequence: string | number;
  crankerRefund: string;
  maxAcceptableRateBps: number;
  /** Optional borrower LTV buffer in bps (default 0): each converted fixed
   *  loan originates at effective_cap = sub-vault max_ltv - buffer. */
  ltvBufferBps?: number;
  simulateOnly?: boolean;
  /** Reproduce the UI's exact bundling path: skip the market LUT, and
   *  drop the SWB precompile+update when a probe-sim says oracles are
   *  fresh. Default false (script uses LUT + always-bundle, mirroring the
   *  working borrower-place-bid pattern). */
  mirrorUi?: boolean;
}

const isSwitchboard = (s: OracleSetup): boolean => s === OracleSetup.SwitchboardPull;

async function main(): Promise<void> {
  const input = readJson<Input>('borrower-convert-p2pool-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const borrower = loadBorrower();
  const marketPk = new PublicKey(market.market);
  const debtMint = new PublicKey(market.debtMint);
  const debtBank = new PublicKey(market.debtBank);
  const collateralBank = new PublicKey(market.collateralBank);

  // Discovery mode — dump every P2Pool loan the borrower owns on this
  // market so the operator can fill the input file. We do this BEFORE the
  // bank reads so a misconfigured input doesn't burn RPC.
  if (String(input.loanSequence).toLowerCase() === 'list') {
    await listBorrowerP2PoolLoans(conn, marketPk, borrower.publicKey);
    return;
  }

  const sequence = BigInt(input.loanSequence.toString());
  log(
    `[borrower-convert-p2pool] market=${market.market} seq=${sequence} ` +
      `maxRate=${input.maxAcceptableRateBps}bps borrower=${borrower.publicKey.toBase58()}`,
  );

  // Re-read bank state every run so oracle setup + liquidity-vault PDAs
  // reflect "now". (Mirror what the UI hook does.)
  const [debtBankState, collateralBankState] = await Promise.all([
    readBankOnchain(conn, debtBank),
    readBankOnchain(conn, collateralBank),
  ]);

  // Convert reads both bank oracles for the per-cross LTV gate. Switchboard
  // pull feeds need to be refreshed in the SAME tx — bundle the update ix
  // in front. Pyth feeds need a separate upfront crank tx (not handled
  // here yet — current mainnet markets are SwitchboardPull on both legs).
  const sbOracleKeys: PublicKey[] = [];
  if (isSwitchboard(debtBankState.oracleSetup)) sbOracleKeys.push(debtBankState.oracleKeys[0]);
  if (isSwitchboard(collateralBankState.oracleSetup))
    sbOracleKeys.push(collateralBankState.oracleKeys[0]);

  // UI-mirror mode: probe-sim WITHOUT the SWB bundle. If the convert ix
  // alone is acceptable, skip the bundle (UI's `simNeedsOracleRefresh`).
  let bundleSwb = true;
  if (input.mirrorUi) {
    log(`[borrower-convert-p2pool] mirror-ui: probing convert alone for oracle staleness…`);
    const probeIxs: TransactionInstruction[] = [
      ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }),
      // Build convert ix inline for the probe (we rebuild below for real send).
    ];
    // Build a throwaway convert ix to probe with.
    probeIxs.push(
      convertP2poolToFixedInstruction({
        borrower: borrower.publicKey,
        market: marketPk,
        loanSequence: BigInt(input.loanSequence.toString()),
        debtMint,
        debtBank,
        debtLiquidityVault: debtBankState.liquidityVault,
        debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(debtBank),
        debtOracles: bankOracleAccounts(debtBankState),
        collateralBank,
        collateralOracles: bankOracleAccounts(collateralBankState),
        tokenProgram: TOKEN_PROGRAM_ID,
        marginfiGroup: new PublicKey(market.marginfiGroup),
        marginfiProgram: MARGINFI_PROGRAM_ID,
        maxAcceptableRateBps: input.maxAcceptableRateBps,
        ltvBufferBps: input.ltvBufferBps,
        crankerRefund: new PublicKey(input.crankerRefund),
      }),
    );
    const probeNeedRefresh = await probeConvertNeedsOracleRefresh(
      conn,
      borrower.publicKey,
      probeIxs,
    );
    bundleSwb = probeNeedRefresh;
    log(`[borrower-convert-p2pool] mirror-ui: probe says needRefresh=${probeNeedRefresh}`);
  }
  const { ixs: sbIxs, luts: sbLuts } = bundleSwb
    ? await buildSwitchboardUpdateIxs(conn, borrower, sbOracleKeys)
    : { ixs: [] as TransactionInstruction[], luts: [] as AddressLookupTableAccount[] };
  log(`[borrower-convert-p2pool] bundled ${sbIxs.length} switchboard ix(s) + ${sbLuts.length} LUT(s)`);

  // mirrorUi mode: skip the market LUT (UI sets skipMarketLut: true).
  let marketLut: AddressLookupTableAccount | null = null;
  if (!input.mirrorUi) {
    if (!market.lookupTable) {
      throw new Error(
        `markets.json[${market.label}] has no lookupTable — run \`yarn create-market-lut\` first`,
      );
    }
    const lutRes = await conn.getAddressLookupTable(new PublicKey(market.lookupTable));
    if (!lutRes.value) {
      throw new Error(`market LUT ${market.lookupTable} not found on-chain (not yet active?)`);
    }
    marketLut = lutRes.value;
  } else {
    log(`[borrower-convert-p2pool] mirror-ui: market LUT skipped`);
  }

  // No off-chain prediction needed — the program runs the full
  // `calc_interest_rate` flow at `Clock::now()` and sizes funding to the
  // exact post-accrue liability for THIS loan's share slice.
  const convertIx = convertP2poolToFixedInstruction({
    borrower: borrower.publicKey,
    market: marketPk,
    loanSequence: sequence,
    debtMint,
    debtBank,
    debtLiquidityVault: debtBankState.liquidityVault,
    debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(debtBank),
    debtOracles: bankOracleAccounts(debtBankState),
    collateralBank,
    collateralOracles: bankOracleAccounts(collateralBankState),
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    marginfiProgram: MARGINFI_PROGRAM_ID,
    maxAcceptableRateBps: input.maxAcceptableRateBps,
    ltvBufferBps: input.ltvBufferBps,
    crankerRefund: new PublicKey(input.crankerRefund),
  });

  // Switchboard ixs MUST lead the tx (precompile references update by
  // absolute instruction index). Order: SWB, CU, convert.
  const ixs: TransactionInstruction[] = [
    ...sbIxs,
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }),
    ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }),
    convertIx,
  ];

  // Dump the bundled tx layout so we can correlate "Error processing
  // Instruction N" against what's actually at N. Wallet adapter logs in
  // the UI hide this.
  log(`[borrower-convert-p2pool] tx layout (${ixs.length} ix):`);
  ixs.forEach((ix, i) => {
    log(`  [${i}] ${ix.programId.toBase58()} (keys=${ix.keys.length} data=${ix.data.length}B)`);
  });

  const luts: AddressLookupTableAccount[] = [
    ...(marketLut ? [marketLut] : []),
    ...sbLuts,
  ];

  if (input.simulateOnly) {
    await simulateAndDump(conn, borrower.publicKey, ixs, luts);
    return;
  }

  assertSendAllowed(conn);
  const sig = await sendVerboseV0(conn, borrower, ixs, luts);
  log(`[borrower-convert-p2pool] signature = ${sig}`);
  appendTxLog({
    script: 'borrower-convert-p2pool',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      borrower: borrower.publicKey.toBase58(),
      loanSequence: sequence.toString(),
      maxAcceptableRateBps: input.maxAcceptableRateBps,
      ltvBufferBps: input.ltvBufferBps ?? 0,
      switchboardBundled: sbIxs.length,
    },
  });
}

/** Simulate-only — same path the on-chain RPC takes during preflight, but
 *  we dump the FULL logs even on success (UI is missing the program-side
 *  log on failure, which is the actual diagnostic). */
async function simulateAndDump(
  conn: Connection,
  payer: PublicKey,
  ixs: TransactionInstruction[],
  luts: AddressLookupTableAccount[],
): Promise<void> {
  const { blockhash } = await conn.getLatestBlockhash('confirmed');
  const msg = new TransactionMessage({
    payerKey: payer,
    recentBlockhash: blockhash,
    instructions: ixs,
  }).compileToV0Message(luts);
  const tx = new VersionedTransaction(msg);
  const sim = await conn.simulateTransaction(tx, {
    sigVerify: false,
    replaceRecentBlockhash: true,
    commitment: 'confirmed',
  });
  log(`[borrower-convert-p2pool] simulate err=${JSON.stringify(sim.value.err)}`);
  log(`[borrower-convert-p2pool] simulate unitsConsumed=${sim.value.unitsConsumed ?? 'n/a'}`);
  const logsArr = sim.value.logs ?? [];
  log(`[borrower-convert-p2pool] simulate logs (${logsArr.length}):`);
  for (const line of logsArr) log(`  ${line}`);
  if (sim.value.err) process.exitCode = 1;
}

/** Build/sign/send v0 with FULL preflight log capture on failure — the
 *  default `sendV0Ixs` swallows logs into the bare error string. */
async function sendVerboseV0(
  conn: Connection,
  payer: ReturnType<typeof loadBorrower>,
  ixs: TransactionInstruction[],
  luts: AddressLookupTableAccount[],
): Promise<string> {
  const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash('confirmed');
  const msg = new TransactionMessage({
    payerKey: payer.publicKey,
    recentBlockhash: blockhash,
    instructions: ixs,
  }).compileToV0Message(luts);
  const tx = new VersionedTransaction(msg);
  tx.sign([payer]);
  try {
    const sig = await conn.sendRawTransaction(tx.serialize(), {
      skipPreflight: false,
      preflightCommitment: 'confirmed',
    });
    const conf = await conn.confirmTransaction(
      { signature: sig, blockhash, lastValidBlockHeight },
      'confirmed',
    );
    if (conf.value.err) {
      throw new Error(`tx ${sig} failed in-flight: ${JSON.stringify(conf.value.err)}`);
    }
    return sig;
  } catch (e) {
    if (e instanceof SendTransactionError) {
      // `getLogs(conn)` exists at runtime in @solana/web3.js but isn't in
      // the public type — cast through unknown for the call.
      const getLogs = (e as unknown as { getLogs?: (c: Connection) => Promise<string[] | null> })
        .getLogs;
      const logs = getLogs ? await getLogs.call(e, conn).catch(() => null) : e.logs ?? null;
      log(`[borrower-convert-p2pool] preflight rejected — FULL LOGS:`);
      for (const line of logs ?? []) log(`  ${line}`);
    }
    throw e;
  }
}

/** Exact port of `lib/sendBundled.ts::simNeedsOracleRefresh` — simulate
 *  the consumer ixs WITHOUT the SWB bundle to see whether the convert
 *  would reject on `Custom 102` (stale oracle). Returns true conservatively
 *  on any RPC error. We mirror this to confirm the UI's probe decision
 *  matches what we observe locally. */
async function probeConvertNeedsOracleRefresh(
  conn: Connection,
  payer: PublicKey,
  ixs: TransactionInstruction[],
): Promise<boolean> {
  try {
    const { blockhash } = await conn.getLatestBlockhash('confirmed');
    const msg = new TransactionMessage({
      payerKey: payer,
      recentBlockhash: blockhash,
      instructions: ixs,
    }).compileToV0Message();
    const tx = new VersionedTransaction(msg);
    const sim = await conn.simulateTransaction(tx, {
      sigVerify: false,
      replaceRecentBlockhash: true,
      commitment: 'confirmed',
    });
    if (!sim.value.err) return false;
    const errStr = JSON.stringify(sim.value.err);
    const logs = (sim.value.logs ?? []).join('\n');
    log(`[probe] err=${errStr}`);
    log(`[probe] logs:`);
    for (const line of sim.value.logs ?? []) log(`  ${line}`);
    return /"Custom":\s*102/.test(errStr) || /OracleStale|stale|oracle/i.test(logs);
  } catch (e) {
    log(`[probe] threw ${e instanceof Error ? e.message : String(e)}`);
    return true;
  }
}

/** Mirror of the UI's `useActiveLoans` discovery: scan every LoanFixed,
 *  resolve borrower via the market's claimed-seat tree, and print P2Pool
 *  hits with everything the operator needs to fill the input file
 *  (sequence, crankerRefund, outstandingDebtAtoms, maturity). */
async function listBorrowerP2PoolLoans(
  conn: Connection,
  marketPk: PublicKey,
  borrower: PublicKey,
): Promise<void> {
  log(`[discover] scanning loans for ${borrower.toBase58()} on ${marketPk.toBase58()}…`);
  const marketInfo = await conn.getAccountInfo(marketPk);
  if (!marketInfo) throw new Error(`market ${marketPk.toBase58()} not found`);
  const market = decodeMarket(marketInfo.data);

  // Loans on this market: filter by data size + discriminant. Same pattern
  // as `useActiveLoans` in hooks/portfolio.ts.
  const discBytes = new Uint8Array(8);
  new DataView(discBytes.buffer).setBigUint64(0, LOAN_FIXED_DISCRIMINANT, true);
  const accts = await conn.getProgramAccounts(YDELTA_PROGRAM_ID, {
    filters: [
      { dataSize: LOAN_FIXED_SIZE },
      { memcmp: { offset: 0, bytes: bs58.encode(discBytes) } },
    ],
  });
  const seatByIdx = new Map<number, (typeof market.claimedSeats)[number]['seat']>();
  for (const s of market.claimedSeats) seatByIdx.set(s.index, s.seat);

  const hits: Array<{
    loanPk: PublicKey;
    sequence: bigint;
    createdBy: PublicKey;
    outstandingDebtAtoms: bigint;
    collateralAtoms: bigint;
    maturesAtUnix: bigint;
    loanType: LoanType;
  }> = [];
  for (const { pubkey, account } of accts) {
    try {
      const loan = decodeLoanFixed(account.data);
      if (!loan.market.equals(marketPk)) continue;
      const seat = seatByIdx.get(loan.borrowerSeatIndex);
      if (!seat || !seat.owner.equals(borrower)) continue;
      hits.push({
        loanPk: pubkey,
        sequence: loan.matchedLoanSequence,
        createdBy: loan.createdBy,
        outstandingDebtAtoms: loan.outstandingDebtAtoms,
        collateralAtoms: loan.collateralAtoms,
        maturesAtUnix: loan.maturesAtUnix,
        loanType: loan.loanType,
      });
    } catch {
      /* skip undecodable */
    }
  }
  hits.sort((a, b) => Number(a.maturesAtUnix - b.maturesAtUnix));

  const p2pool = hits.filter((h) => h.loanType === LoanType.P2Pool);
  log(`[discover] ${hits.length} total loans, ${p2pool.length} P2Pool`);
  for (const h of p2pool) {
    log('  ─────────');
    log(`  loanPda           : ${h.loanPk.toBase58()}`);
    log(`  loanSequence      : ${h.sequence.toString()}`);
    log(`  crankerRefund     : ${h.createdBy.toBase58()}`);
    log(`  outstandingDebt   : ${h.outstandingDebtAtoms.toString()} atoms (stale for P2Pool)`);
    log(`  collateral        : ${h.collateralAtoms.toString()} atoms`);
    log(`  maturesAtUnix     : ${h.maturesAtUnix.toString()}  (${new Date(Number(h.maturesAtUnix) * 1000).toISOString()})`);
  }
  if (p2pool.length === 0) {
    log('  (no P2Pool loans on this market — nothing to refinance)');
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
