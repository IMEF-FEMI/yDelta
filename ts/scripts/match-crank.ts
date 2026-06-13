/**
 * match-crank.ts — `MatchCrank` (tag 43, v1 D7/D8). Permissionless crank
 * that walks resting asks against resting bids, filling up to `maxFills`
 * crosses per call (`0` is treated as `1`). Anyone may sign.
 *
 * Reads:
 *   .local/match-crank-input.json {
 *     marketLabel: string,
 *     maxFills?: number                      // crosses per call; default 1
 *   }
 *
 * Like `borrower-place-bid`, the matcher reads BOTH bank oracles for the
 * per-cross LTV gate, so we crank both banks here before sending (Pyth in a
 * separate upfront tx, Switchboard bundled into the crank tx). The `vault`
 * is derived on-chain from `debtBank` (the market's debt lending pool).
 * Wraps the ix with the heavy-CU preamble.
 */
import { ComputeBudgetProgram, PublicKey } from '@solana/web3.js';

import { matchCrankInstruction } from '../src/instructions/index.js';
import { OracleSetup } from '../src/marginfi.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  sendV0Ixs,
} from './_runner.js';
import { readBankOnchain } from './_marginfi.js';
import { buildSwitchboardUpdateIxs, crankStaleBankOracles } from './_oracleCrank.js';
import type { MarketDump } from './_types.js';

const isPyth = (s: OracleSetup) =>
  s === OracleSetup.PythPushOracle || s === OracleSetup.StakedWithPythPush;
const isSwitchboard = (s: OracleSetup) => s === OracleSetup.SwitchboardPull;

interface Input {
  marketLabel: string;
  maxFills?: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('match-crank-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const signer = loadSigner();

  const marketPk = new PublicKey(market.market);
  const debtBank = new PublicKey(market.debtBank);
  const collateralBank = new PublicKey(market.collateralBank);
  const debtOracles = market.debtOracles.map((s) => new PublicKey(s));
  const collateralOracles = market.collateralOracles.map((s) => new PublicKey(s));

  // We re-read bank state every run so oracle staleness reflects "now".
  const debtBankState = await readBankOnchain(conn, debtBank);
  const collateralBankState = await readBankOnchain(conn, collateralBank);

  // Pyth feeds: crank conditionally in a separate upfront tx (skip when
  // fresh; a Pyth post is multi-tx so it can't be bundled).
  const crank = await crankStaleBankOracles(
    conn,
    signer,
    [
      { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
      { bank: collateralBankState, pythFeedIdHex: market.collateralPythFeedIdHex, pythShardId: market.collateralPythShardId },
    ].filter((b) => isPyth(b.bank.oracleSetup)),
  );

  // Switchboard feeds: always build an update ix and bundle it INTO the
  // match-crank tx so the fresh price and the LTV cross execute atomically.
  const sbOracleKeys: PublicKey[] = [];
  if (isSwitchboard(debtBankState.oracleSetup)) sbOracleKeys.push(debtBankState.oracleKeys[0]);
  if (isSwitchboard(collateralBankState.oracleSetup)) sbOracleKeys.push(collateralBankState.oracleKeys[0]);
  const { ixs: sbIxs, luts: sbLuts } = await buildSwitchboardUpdateIxs(conn, signer, sbOracleKeys);

  // The market LUT compresses match_crank's account list so the bundle fits
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

  const maxFills = input.maxFills ?? 1;
  log(`[match-crank] market=${marketPk.toBase58()} maxFills=${maxFills} signer=${signer.publicKey.toBase58()}`);

  const ix = matchCrankInstruction({
    payer: signer.publicKey,
    market: marketPk,
    debtBank,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    collateralBank,
    debtOracles,
    collateralOracles,
    maxFills,
  });

  // Switchboard ixs MUST lead the tx (their precompile references data by
  // absolute instruction index). Order: SB, CU, crank.
  const cuIxs = [
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }),
    ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }),
  ];
  const sig = await sendV0Ixs(
    conn,
    signer,
    [...sbIxs, ...cuIxs, ix],
    { luts: [lutRes.value, ...sbLuts] },
  );
  log(`[match-crank] signature = ${sig} (bundled ${sbIxs.length} switchboard ix)`);
  appendTxLog({
    script: 'match-crank',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      maxFills,
      cranker: signer.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
