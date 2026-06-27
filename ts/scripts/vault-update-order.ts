/**
 * vault-update-order.ts — `UpdateOrderForSubVault` (tag 13). Curator re-syncs
 * (cancel + re-post) a vault ask: it loses price-time priority and re-quotes
 * at the current `live bank APR + subVault.spreadBps`.
 *
 * Re-posting walks the bid tree exactly like a fresh place, so when the
 * re-quoted ask crosses a resting bid the processor reads BOTH the debt and
 * collateral marginfi oracles to gate the cross on LTV — a stale feed reverts
 * the whole tx with Custom 102 (AdapterError::OracleStale). Refresh exactly
 * like vault-place-ask / borrower-place-bid: crank Pyth-push upfront when
 * stale, bundle the Switchboard-pull update INTO this tx (market LUT compresses
 * the account list under the 1232-byte limit).
 *
 * Reads:
 *   .local/vault-update-order-input.json {
 *     marketLabel: string,                   // .local/markets.json key
 *     mint: string,                          // debt mint (must equal market.debtMint)
 *     subVaultId: number,
 *     newFlags?: number
 *   }
 *   .local/markets.json, sub-vaults.json, curators.json
 *
 * Fee payer = deployer (signer); curator co-signs.
 */
import { ComputeBudgetProgram, PublicKey } from '@solana/web3.js';

import { updateOrderForSubVaultInstruction } from '../src/instructions/index.js';
import { OracleSetup } from '../src/marginfi.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  readKeypairFromBase58,
  sendV0Ixs,
} from './_runner.js';
import { readBankOnchain } from './_marginfi.js';
import { buildSwitchboardUpdateIxs, crankStaleBankOracles } from './_oracleCrank.js';
import type { MarketDump, SubVaultDump, CuratorDump } from './_types.js';
import { resolveCuratorForSubVault } from './_types.js';

const isPyth = (s: OracleSetup) =>
  s === OracleSetup.PythPushOracle || s === OracleSetup.StakedWithPythPush;
const isSwitchboard = (s: OracleSetup) => s === OracleSetup.SwitchboardPull;

interface Input {
  marketLabel: string;
  mint: string;
  subVaultId: number;
  newFlags?: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('vault-update-order-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const subVaults = readJson<Record<string, SubVaultDump[]>>('sub-vaults.json');
  const curators = readJson<CuratorDump[]>('curators.json');

  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);
  if (market.debtMint !== input.mint) {
    throw new Error(
      `market ${input.marketLabel} debtMint ${market.debtMint} ≠ input.mint ${input.mint}`,
    );
  }
  const curator = resolveCuratorForSubVault(subVaults, curators, input.mint, input.subVaultId);
  const curatorKp = readKeypairFromBase58(curator.secretKeyBase58);

  const conn = loadConnection();
  const feePayer = loadSigner();

  const marketPk = new PublicKey(market.market);
  const debtBank = new PublicKey(market.debtBank);
  const collateralBank = new PublicKey(market.collateralBank);
  const debtOracles = market.debtOracles.map((o) => new PublicKey(o));
  const collateralOracles = market.collateralOracles.map((o) => new PublicKey(o));

  const debtBankState = await readBankOnchain(conn, debtBank);
  const collateralBankState = await readBankOnchain(conn, collateralBank);

  const crank = await crankStaleBankOracles(
    conn,
    feePayer,
    [
      { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
      { bank: collateralBankState, pythFeedIdHex: market.collateralPythFeedIdHex, pythShardId: market.collateralPythShardId },
    ].filter((b) => isPyth(b.bank.oracleSetup)),
  );

  const sbOracleKeys: PublicKey[] = [];
  if (isSwitchboard(debtBankState.oracleSetup)) sbOracleKeys.push(debtBankState.oracleKeys[0]);
  if (isSwitchboard(collateralBankState.oracleSetup)) sbOracleKeys.push(collateralBankState.oracleKeys[0]);
  const { ixs: sbIxs, luts: sbLuts } = await buildSwitchboardUpdateIxs(conn, feePayer, sbOracleKeys);

  if (!market.lookupTable) {
    throw new Error(
      `markets.json[${market.label}] has no lookupTable — run \`yarn create-market-lut\` first`,
    );
  }
  const lutRes = await conn.getAddressLookupTable(new PublicKey(market.lookupTable));
  if (!lutRes.value) {
    throw new Error(`market LUT ${market.lookupTable} not found on-chain (not yet active?)`);
  }

  const ix = updateOrderForSubVaultInstruction({
    feePayer: feePayer.publicKey,
    curator: curatorKp.publicKey,
    market: marketPk,
    debtBank,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    collateralBank,
    debtOracles,
    collateralOracles,
    subVaultId: input.subVaultId,
    newFlags: input.newFlags,
  });

  log(
    `[vault-update-order] market=${marketPk.toBase58()} subVaultId=${input.subVaultId} ` +
    `curator=${curatorKp.publicKey.toBase58()} (cranked ${crank.entries.length} pyth, ` +
    `bundling ${sbIxs.length} switchboard ix)`,
  );

  // Switchboard ixs MUST lead the tx (precompile references data by absolute
  // instruction index). Order: SB, CU, update-order.
  const cuIxs = [
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }),
    ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }),
  ];
  const sig = await sendV0Ixs(
    conn,
    feePayer,
    [...sbIxs, ...cuIxs, ix],
    { luts: [lutRes.value, ...sbLuts], extraSigners: [curatorKp] },
  );
  log(`[vault-update-order] signature = ${sig} (bundled ${sbIxs.length} switchboard ix)`);
  appendTxLog({
    script: 'vault-update-order',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      mint: input.mint,
      subVaultId: input.subVaultId,
      curator: curatorKp.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
