/**
 * seat-withdraw.ts — `Withdraw` (tag 3). Trader withdraws atoms from
 * their seat on a market. `withdrawAll: true` drains the full
 * withdrawable balance on the selected side (ignores `amountAtoms`).
 *
 * Reads:
 *   .local/seat-withdraw-input.json {
 *     marketLabel: string,
 *     mint: string,                          // debt or collateral mint
 *     amountAtoms?: string | number,         // required unless withdrawAll
 *     withdrawAll?: boolean,
 *     traderIndexHint?: number | null
 *   }
 *
 * Withdraw runs a marginfi health check, so we crank BOTH the debt and
 * collateral bank oracles (the ix passes both account groups). Order:
 * debt oracles then collateral oracles (matches the on-chain expected
 * account layout).
 */
import { ComputeBudgetProgram, PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import { withdrawInstruction } from '../src/instructions/index.js';
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
  bankLiquidityVaultAuthority,
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
  mint: string;
  amountAtoms?: string | number;
  withdrawAll?: boolean;
  traderIndexHint?: number | null;
}

async function main(): Promise<void> {
  const input = readJson<Input>('seat-withdraw-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const trader = loadBorrower();

  const sideMint = new PublicKey(input.mint);
  const debtMint = new PublicKey(market.debtMint);
  const isDebt = sideMint.equals(debtMint);
  if (!isDebt && !sideMint.equals(new PublicKey(market.collateralMint))) {
    throw new Error(
      `mint ${input.mint} matches neither debt nor collateral of ${market.label}`,
    );
  }

  const debtBank = new PublicKey(market.debtBank);
  const collateralBank = new PublicKey(market.collateralBank);
  const sideBank = isDebt ? debtBank : collateralBank;
  const sideLiquidityVault = new PublicKey(
    isDebt ? market.debtLiquidityVault : market.collateralLiquidityVault,
  );

  // Re-read both banks so we know their oracleSetup right now.
  const debtBankState = await readBankOnchain(conn, debtBank);
  const collateralBankState = await readBankOnchain(conn, collateralBank);

  // Pyth feeds: crank conditionally in a separate upfront tx (skip when
  // fresh; a Pyth post is multi-tx so it can't be bundled).
  const crank = await crankStaleBankOracles(
    conn,
    trader,
    [
      { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
      { bank: collateralBankState, pythFeedIdHex: market.collateralPythFeedIdHex, pythShardId: market.collateralPythShardId },
    ].filter((b) => isPyth(b.bank.oracleSetup)),
  );

  // Switchboard feeds: always build an update ix and bundle it INTO the
  // withdraw tx so the fresh price and the health check execute atomically.
  const sbOracleKeys: PublicKey[] = [];
  if (isSwitchboard(debtBankState.oracleSetup)) sbOracleKeys.push(debtBankState.oracleKeys[0]);
  if (isSwitchboard(collateralBankState.oracleSetup)) sbOracleKeys.push(collateralBankState.oracleKeys[0]);
  const { ixs: sbIxs, luts: sbLuts } = await buildSwitchboardUpdateIxs(conn, trader, sbOracleKeys);

  // The market LUT compresses the withdraw's ~21 accounts so the bundle
  // fits the 1232-byte tx limit.
  if (!market.lookupTable) {
    throw new Error(
      `markets.json[${market.label}] has no lookupTable — run \`yarn create-market-lut\` first`,
    );
  }
  const lutRes = await conn.getAddressLookupTable(new PublicKey(market.lookupTable));
  if (!lutRes.value) {
    throw new Error(`market LUT ${market.lookupTable} not found on-chain (not yet active?)`);
  }

  const debtOracles = market.debtOracles.map((s) => new PublicKey(s));
  const collateralOracles = market.collateralOracles.map((s) => new PublicKey(s));
  let amount: bigint;
  if (input.withdrawAll) {
    // The on-chain `Withdraw` ix uses `withdraw_all=true` to drain the
    // full balance regardless of `amount_atoms`. Pass 0 — the program
    // ignores it on the withdraw-all path.
    amount = 0n;
  } else {
    if (input.amountAtoms === undefined) {
      throw new Error('seat-withdraw-input.json: either amountAtoms or withdrawAll required');
    }
    amount = BigInt(input.amountAtoms.toString());
  }

  log(
    `[seat-withdraw] ${market.label} side=${isDebt ? 'debt' : 'collateral'} ` +
    `amount=${amount} withdrawAll=${input.withdrawAll ?? false}`,
  );

  const ix = withdrawInstruction({
    payer: trader.publicKey,
    market: new PublicKey(market.market),
    mint: sideMint,
    debtMint,
    traderToken: ataFor(trader.publicKey, sideMint),
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    debtBank,
    collateralBank,
    liquidityVault: sideLiquidityVault,
    bankLiquidityVaultAuthority: bankLiquidityVaultAuthority(sideBank),
    debtOracles,
    collateralOracles,
    marginfiProgram: MARGINFI_PROGRAM_ID,
    amountAtoms: amount,
    traderIndexHint: input.traderIndexHint ?? null,
    withdrawAll: input.withdrawAll ?? false,
  });
  const cuIxs = [
    ComputeBudgetProgram.setComputeUnitLimit({ units: 800_000 }),
    ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }),
  ];
  // Switchboard's update references its sig-verify precompile by ABSOLUTE
  // instruction index, so the SB ixs MUST lead the tx — prepending the CU
  // ixs would shift the precompile and fail the update. Order: SB, CU,
  // ensure-destination-ATA, consumer.
  const sig = await sendV0Ixs(
    conn,
    trader,
    [...sbIxs, ...cuIxs, createAtaIdempotentIx(trader.publicKey, trader.publicKey, sideMint), ix],
    { luts: [lutRes.value, ...sbLuts] },
  );
  log(`[seat-withdraw] signature = ${sig} (bundled ${sbIxs.length} switchboard ix)`);
  appendTxLog({
    script: 'seat-withdraw',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      side: isDebt ? 'debt' : 'collateral',
      amountAtoms: amount.toString(),
      withdrawAll: input.withdrawAll ?? false,
      trader: trader.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
