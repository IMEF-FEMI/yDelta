/**
 * debug-borrow-ltv.ts — reproduce the markets-page borrow calc against
 * mainnet state and report what the on-chain matcher would see.
 *
 * Usage:
 *   yarn tsx ts/scripts/debug-borrow-ltv.ts \
 *     --market <pubkey> [--borrow-ui 200] [--rate-bps 800] [--term-days 30]
 *
 * Reads the live MarketFixed + GlobalVault + debt/coll banks + oracles via
 * RPC, runs our `computeBreakdown` math (mirrored inline so this stays
 * close to the UI logic), then for every per-source part simulates what the
 * on-chain matcher would compute for matched_collateral vs required at
 * gate A (marginfi weights) and gate B (profile cap). Prints a per-fill
 * pass/fail table so we can see exactly where the deficit lives.
 */
import 'dotenv/config';

import { Connection, PublicKey } from '@solana/web3.js';

import {
  decodeBank,
  decodeMarket,
  effectiveMaxLtvBpsForProfile,
  fetchVault,
  isLtvAuto,
  marginfiImpliedMaxLtvBps,
  readOraclePriceFp48,
  requiredCollateralAtoms,
  simulatePlaceOrder,
  uiToAtoms,
  type Bank,
} from '../src/index.js';

const argv = process.argv.slice(2);
const arg = (k: string): string | undefined => {
  const i = argv.indexOf(k);
  return i >= 0 ? argv[i + 1] : undefined;
};

const MARKET = new PublicKey(arg('--market') ?? '5rtJ4GKyGqV2gZUv4RRLTiNzZxDxkETY7QFZnnCn37Ck'); // USDC/JitoSOL by default
const BORROW_UI = Number(arg('--borrow-ui') ?? '200');
const RATE_BPS = Number(arg('--rate-bps') ?? '800');
const TERM_DAYS = Number(arg('--term-days') ?? '30');
const ALLOW_BACKFILL = (arg('--no-backfill') ?? 'on') !== 'off';

const RPC_URL =
  process.env.YDELTA_RPC_URL ??
  process.env.RPC_URL ??
  'https://api.mainnet-beta.solana.com';

async function readPrice(conn: Connection, bank: Bank | null): Promise<bigint | null> {
  if (!bank || bank.oracleKeys.length === 0) return null;
  const info = await conn.getAccountInfo(bank.oracleKeys[0]);
  if (!info) return null;
  return readOraclePriceFp48(bank.oracleSetup, info.data);
}

async function main() {
  const conn = new Connection(RPC_URL, 'confirmed');
  console.log(`RPC: ${RPC_URL}`);
  console.log(`Market: ${MARKET.toBase58()}`);

  const info = await conn.getAccountInfo(MARKET, 'confirmed');
  if (!info) throw new Error('market not found');
  const marketData = info.data;
  const market = decodeMarket(marketData);
  const h = market.header;

  console.log(`Debt: ${h.debtMint.toBase58()} (${h.debtMintDecimals} dec)`);
  console.log(`Coll: ${h.collateralMint.toBase58()} (${h.collateralMintDecimals} dec)`);

  const [vault, bankInfos] = await Promise.all([
    fetchVault(conn, h.debtMint),
    conn.getMultipleAccountsInfo([h.debtLendingPool, h.collateralLendingPool]),
  ]);
  if (!vault) throw new Error('vault not found');
  const debtBank = bankInfos[0] ? decodeBank(bankInfos[0].data) : null;
  const collBank = bankInfos[1] ? decodeBank(bankInfos[1].data) : null;
  if (!debtBank || !collBank) throw new Error('bank decode failed');
  const [debtPriceFp48, collPriceFp48] = await Promise.all([
    readPrice(conn, debtBank),
    readPrice(conn, collBank),
  ]);
  if (debtPriceFp48 == null || collPriceFp48 == null) throw new Error('oracle price missing');

  const a = {
    debtPriceFp48,
    collateralPriceFp48: collPriceFp48,
    liabilityWeightInitFp48: debtBank.liabilityWeightInitFp48,
    collateralAssetWeightInitFp48: collBank.assetWeightInitFp48,
    ltvBufferBps: h.feeConfig.ltvBufferBps,
    debtMintDecimals: h.debtMintDecimals,
    collateralMintDecimals: h.collateralMintDecimals,
  };

  console.log(`\nMarket inputs:`);
  console.log(`  debt_price (USD):   ${Number(a.debtPriceFp48) / 2 ** 48}`);
  console.log(`  coll_price (USD):   ${Number(a.collateralPriceFp48) / 2 ** 48}`);
  console.log(`  liab_weight_init:   ${Number(a.liabilityWeightInitFp48) / 2 ** 48}`);
  console.log(`  asset_weight_init:  ${Number(a.collateralAssetWeightInitFp48) / 2 ** 48}`);
  console.log(`  ltv_buffer_bps:     ${a.ltvBufferBps}`);

  const marginfiImplied = marginfiImpliedMaxLtvBps(
    a.collateralAssetWeightInitFp48,
    a.liabilityWeightInitFp48,
  );
  console.log(`  marginfi-implied LTV (asset/liab × 10000): ${marginfiImplied}bps`);

  console.log(`\nPool profiles in vault:`);
  for (const { profile } of vault.riskProfiles) {
    const eff = effectiveMaxLtvBpsForProfile(
      profile.maxLtvBps,
      a.collateralAssetWeightInitFp48,
      a.liabilityWeightInitFp48,
    );
    const idle = profile.totalPrincipalAtoms - profile.deployedPrincipalAtoms - profile.encumberedInOrdersAtoms;
    console.log(
      `  Pool #${profile.profileId} maxLtv=${profile.maxLtvBps} (effective=${eff}${isLtvAuto(profile.maxLtvBps) ? ' auto' : ''}) idle=${idle}`,
    );
  }

  const borrowAtoms = uiToAtoms(BORROW_UI, h.debtMintDecimals);
  const bidRateBps = RATE_BPS;
  const bidTermSeconds = TERM_DAYS * 86400;
  console.log(
    `\nBid: borrow=${BORROW_UI} (${borrowAtoms} atoms), rate=${bidRateBps}bps, term=${bidTermSeconds}s, backfill=${ALLOW_BACKFILL}`,
  );

  // Run sim with LTV bypassed (matches our breakdown helper).
  const sim = simulatePlaceOrder({
    market,
    marketAccountData: marketData,
    vault,
    bidRateBps,
    bidTermSeconds,
    bidPrincipalAtoms: borrowAtoms,
    bidCollateralAtoms: 1n << 100n,
    obOnly: !ALLOW_BACKFILL,
    ltvEstimator: () => 0,
  });

  console.log(`\nsim.fills: ${sim.fills.length}, residual: ${sim.residualPrincipalAtoms}`);
  for (const f of sim.fills) {
    console.log(
      `  fill profile=${f.profileId} principal=${f.matchedPrincipalAtoms} rate=${f.askRateBps} term=${f.askTermSeconds}`,
    );
  }

  // Build the per-part requireds.
  const FP48_ONE = 1n << 48n;
  type Part = {
    label: string;
    principal: bigint;
    reqAtGateA: bigint;
    reqAtGateB: bigint;
    reqMax: bigint;
    effLtvBps: number;
  };
  const parts: Part[] = [];

  for (const fill of sim.fills) {
    const profile = vault.riskProfiles.find((p) => p.profile.profileId === fill.profileId)?.profile;
    if (!profile) continue;
    const effLtv = effectiveMaxLtvBpsForProfile(
      profile.maxLtvBps,
      a.collateralAssetWeightInitFp48,
      a.liabilityWeightInitFp48,
    );
    const reqA = requiredCollateralAtoms({ borrowAtoms: fill.matchedPrincipalAtoms, ...a });
    let reqB = 0n;
    if (effLtv > 0) {
      const assetW = (BigInt(effLtv) << 48n) / 10_000n;
      reqB = requiredCollateralAtoms({
        borrowAtoms: fill.matchedPrincipalAtoms,
        debtPriceFp48: a.debtPriceFp48,
        collateralPriceFp48: a.collateralPriceFp48,
        liabilityWeightInitFp48: FP48_ONE,
        collateralAssetWeightInitFp48: assetW,
        ltvBufferBps: 0,
        debtMintDecimals: a.debtMintDecimals,
        collateralMintDecimals: a.collateralMintDecimals,
      });
    }
    parts.push({
      label: `Pool#${profile.profileId}${isLtvAuto(profile.maxLtvBps) ? '(auto)' : ''}`,
      principal: fill.matchedPrincipalAtoms,
      reqAtGateA: reqA,
      reqAtGateB: reqB,
      reqMax: reqA > reqB ? reqA : reqB,
      effLtvBps: effLtv,
    });
  }
  if (ALLOW_BACKFILL && sim.residualPrincipalAtoms > 0n) {
    const reqA = requiredCollateralAtoms({ borrowAtoms: sim.residualPrincipalAtoms, ...a });
    parts.push({
      label: 'marginfi-backfill',
      principal: sim.residualPrincipalAtoms,
      reqAtGateA: reqA,
      reqAtGateB: 0n,
      reqMax: reqA,
      effLtvBps: marginfiImplied,
    });
  }

  console.log(`\nPer-part required collateral:`);
  for (const p of parts) {
    console.log(
      `  ${p.label}: principal=${p.principal} reqA=${p.reqAtGateA} reqB=${p.reqAtGateB} reqMax=${p.reqMax} effLtv=${p.effLtvBps}bps`,
    );
  }

  // Compute T with FLOOR (current code) vs CEIL (proper) and slacked variants.
  const computeT = (mode: 'floor' | 'ceil', slackBps: number): bigint => {
    let T = 0n;
    for (const p of parts) {
      if (p.principal === 0n) continue;
      const num = p.reqMax * borrowAtoms;
      const den = p.principal;
      const proRata = mode === 'floor' ? num / den : (num + den - 1n) / den;
      if (proRata > T) T = proRata;
    }
    if (slackBps > 0) {
      T = (T * (10000n + BigInt(slackBps))) / 10000n + 1n;
    }
    return T;
  };

  console.log(`\nT computations:`);
  const cases: Array<{ name: string; T: bigint }> = [
    { name: 'floor + 0 slack (current)', T: computeT('floor', 0) },
    { name: 'ceil  + 0 slack', T: computeT('ceil', 0) },
    { name: 'ceil  + 25 bps slack', T: computeT('ceil', 25) },
    { name: 'ceil  + 50 bps slack', T: computeT('ceil', 50) },
    { name: 'ceil  + 100 bps slack', T: computeT('ceil', 100) },
  ];
  for (const c of cases) {
    console.log(`  ${c.name}: ${c.T}`);
  }

  // Simulate on-chain check for each candidate T.
  console.log(`\nOn-chain check simulation per case (matched = floor(F × T / B)):`);
  for (const c of cases) {
    console.log(`\n  --- ${c.name} (T=${c.T}) ---`);
    let allPass = true;
    for (const p of parts) {
      if (p.principal === 0n) continue;
      const matched = (p.principal * c.T) / borrowAtoms;
      const passA = matched >= p.reqAtGateA;
      const passB = p.reqAtGateB === 0n || matched >= p.reqAtGateB;
      const deficitA = passA ? 0n : p.reqAtGateA - matched;
      const deficitB = passB ? 0n : p.reqAtGateB - matched;
      console.log(
        `    ${p.label}: matched=${matched} reqA=${p.reqAtGateA} ${passA ? 'PASS' : `FAIL(-${deficitA})`} | reqB=${p.reqAtGateB} ${passB ? 'PASS' : `FAIL(-${deficitB})`}`,
      );
      if (!passA || !passB) allPass = false;
    }
    console.log(`  → overall: ${allPass ? 'PASS' : 'FAIL'}`);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
