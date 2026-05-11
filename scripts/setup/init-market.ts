#!/usr/bin/env -S npx ts-node
/* eslint-disable no-console */
import * as fs from 'fs';
import * as path from 'path';
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
} from '@solana/spl-token';
import {
  assertNotDevnet,
  confirmMainnetIfNeeded,
  defaultCluster,
  defaultPayerPath,
  fmtKey,
  header,
  LOCAL_DIR,
  loadDotEnv,
  loadKeypair,
  makeConnection,
  mkdirp,
  optionalArg,
  parseArgs,
  requireArg,
  sendIxs,
  writeJson,
} from './common';
import { MARKET_FIXED_SIZE } from '../../client/ts/src/utils/discriminator';
import { YDELTA_PROGRAM_ID } from '../../client/ts/src/utils/programId';
import { GlobalConfig } from '../../client/ts/src/globalConfig';
import {
  globalConfigPda,
  globalVaultPda,
  marketSignerPda,
  marketVaultPda,
  lenderMarginfiAccountPda,
  borrowerMarginfiAccountPda,
  vaultIntegrationAccountPda,
  vaultStagingPda,
  globalVaultSignerPda,
} from '../../client/ts/src/utils/pdas';
import {
  createCreateMarketInstruction,
  createCreateVaultInstruction,
  createSetMarketPauseInstruction,
} from '../../client/ts/src/ydelta/instructions';
import { Market } from '../../client/ts/src/market';
import { Vault } from '../../client/ts/src/vault';

/* USAGE:
 *
 * Batch mode (config-driven) — preferred when initializing more than one market:
 *   yarn setup:init-market --config .local/markets-config.json
 *
 *   Config shape (see scripts/setup/markets.example.json):
 *   {
 *     "markets": [
 *       {
 *         "name": "USDC/SOL",
 *         "debtMint": "...", "collateralMint": "...",
 *         "marginfiGroup": "...", "debtBank": "...", "collateralBank": "...",
 *         "marketKeypair": "<optional path to keypair.json for retries>",
 *         "skipVault": false,
 *         "unpauseAfterSetup": false
 *       }
 *     ]
 *   }
 *
 * Single-market mode (flag-driven, legacy):
 *   yarn setup:init-market \
 *     --cluster localhost \
 *     --debt-mint <USDC> --collateral-mint <WSOL> \
 *     --marginfi-group <PUBKEY> \
 *     --debt-bank <PUBKEY> --collateral-bank <PUBKEY> \
 *     [--market-key <kp.json>] [--payer <path>] \
 *     [--skip-vault] [--unpause-after-setup] [--out <path>]
 *
 * Idempotent. The market account is fresh per run unless `marketKeypair` /
 * `--market-key` references an existing file. Markets start PAUSED; unpause
 * via SetMarketPause once risk profiles are in place. The debt-mint
 * GlobalVault is created once per mint and shared across markets that use
 * the same debt mint.
 *
 * Batch mode stops on the first failure — succeeded markets remain on-chain
 * and a retry will be idempotent for them; fix the bad spec and re-run.
 */

const MARGINFI_PROGRAM_ID = new PublicKey('MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA');

type MarketSpec = {
  name?: string;
  debtMint: string;
  collateralMint: string;
  marginfiGroup: string;
  debtBank: string;
  collateralBank: string;
  marketKeypair?: string;
  skipVault?: boolean;
  unpauseAfterSetup?: boolean;
};

type MarketsConfig = {
  markets: MarketSpec[];
};

type MarketOutput = {
  cluster: string;
  rpc: string;
  name?: string;
  market: string;
  marketSigner: string;
  /** Tx signer used at CreateMarket time — equals `GlobalConfig.protocol_admin`. */
  protocolAdmin: string;
  debtMint: string;
  collateralMint: string;
  debtVault: string;
  collateralVault: string;
  lenderMarginfiAccount: string;
  borrowerMarginfiAccount: string;
  marginfiGroup: string;
  marginfiProgram: string;
  debtBank: string;
  collateralBank: string;
  globalVault: {
    address: string;
    signer: string;
    integrationAccount: string;
    staging: string;
  };
  createdAtUnix: number;
  txSignatures: {
    createMarket: string;
    createVault?: string;
    unpause?: string;
  };
};

const REQUIRED_SPEC_FIELDS: ReadonlyArray<keyof MarketSpec> = [
  'debtMint',
  'collateralMint',
  'marginfiGroup',
  'debtBank',
  'collateralBank',
];

function loadMarketsConfig(configPath: string): MarketsConfig {
  const raw = fs.readFileSync(configPath, 'utf8');
  const parsed = JSON.parse(raw);
  if (!parsed || !Array.isArray(parsed.markets) || parsed.markets.length === 0) {
    throw new Error(`config ${configPath} must have a non-empty "markets" array`);
  }
  for (let i = 0; i < parsed.markets.length; i++) {
    const m = parsed.markets[i];
    for (const f of REQUIRED_SPEC_FIELDS) {
      if (typeof m[f] !== 'string' || m[f].length === 0) {
        throw new Error(`config ${configPath}: markets[${i}] missing or invalid "${f}"`);
      }
    }
  }
  return parsed as MarketsConfig;
}

function specFromFlags(args: Record<string, string | string[] | boolean>): MarketSpec {
  return {
    name: optionalArg(args, 'name'),
    debtMint: requireArg(args, 'debt-mint'),
    collateralMint: requireArg(args, 'collateral-mint'),
    marginfiGroup: requireArg(args, 'marginfi-group'),
    debtBank: requireArg(args, 'debt-bank'),
    collateralBank: requireArg(args, 'collateral-bank'),
    marketKeypair: optionalArg(args, 'market-key'),
    skipVault: args['skip-vault'] === true,
    unpauseAfterSetup: args['unpause-after-setup'] === true,
  };
}

/** Initialize a single yDelta market (CreateMarket + CreateVault + optional
 *  unpause). Extracted so batch mode can call it in a loop while the
 *  cluster/payer/connection/mainnet-confirmation are set up once. */
async function initOneMarket(
  connection: Connection,
  payer: Keypair,
  cluster: string,
  spec: MarketSpec,
  outPathOverride: string | undefined,
): Promise<MarketOutput> {
  const debtMint = new PublicKey(spec.debtMint);
  const collateralMint = new PublicKey(spec.collateralMint);
  const marginfiGroup = new PublicKey(spec.marginfiGroup);
  const debtBank = new PublicKey(spec.debtBank);
  const collateralBank = new PublicKey(spec.collateralBank);
  // The on-chain loader hard-codes `marginfi_mocks::ID` for the marginfi
  // program — there is no operator override.
  const marginfiProgram = MARGINFI_PROGRAM_ID;
  const skipVault = spec.skipVault === true;
  const marketKeypair = spec.marketKeypair
    ? loadKeypair(spec.marketKeypair)
    : Keypair.generate();

  const label = spec.name ?? `${fmtKey(debtMint)}/${fmtKey(collateralMint)}`;
  header(`Init market "${label}" → ${cluster}`);
  console.log(`  Payer:       ${fmtKey(payer.publicKey)}`);
  console.log(`  Market key:  ${fmtKey(marketKeypair.publicKey)}`);
  console.log(`  Debt mint:   ${fmtKey(debtMint)}`);
  console.log(`  Coll. mint:  ${fmtKey(collateralMint)}`);
  console.log(`  Debt bank:   ${fmtKey(debtBank)}`);
  console.log(`  Coll. bank:  ${fmtKey(collateralBank)}`);
  console.log(`  MFi group:   ${fmtKey(marginfiGroup)}`);

  // ─── Step 1: CreateMarket (atomic: alloc + init) ──────────────────────
  // Skip if the market account already exists and is owned by ydelta with data.
  // CreateMarket bundles SystemProgram.createAccount + ydelta::CreateMarket
  // into one tx. If the alloc lands but ydelta init fails (e.g. mint
  // extension rejected), the keypair now sits on a 0-data system-owned
  // account; re-running with the SAME marketKeypair will hit the "exists but
  // isn't a ydelta market" branch below. Recovery: either pass a different
  // marketKeypair, or recover rent via `solana account-close <pubkey>` from
  // the keypair, then re-run.
  const existingMarket = await connection.getAccountInfo(marketKeypair.publicKey);
  let createMarketSig: string = '<already-initialized>';
  let skipCreateMarket = false;
  if (existingMarket) {
    if (existingMarket.owner.equals(YDELTA_PROGRAM_ID) && existingMarket.data.length > 0) {
      console.log(`  ✓ Market already initialized at ${fmtKey(marketKeypair.publicKey)}, skipping`);
      skipCreateMarket = true;
    } else {
      throw new Error(
        `account ${fmtKey(marketKeypair.publicKey)} exists but isn't a ydelta market ` +
          `(owner=${fmtKey(existingMarket.owner)}, lamports=${existingMarket.lamports}); refusing to overwrite. ` +
          `To recover rent from this stranded alloc: \`solana account-close ${fmtKey(marketKeypair.publicKey)}\` ` +
          `from the keypair file, then re-run.`,
      );
    }
  }

  // GlobalConfig + protocol-admin preflight. Done per-market so a missing
  // GlobalConfig fails fast on the first iteration with a useful message.
  const [globalConfigAddr] = globalConfigPda();
  const gcInfo = await connection.getAccountInfo(globalConfigAddr);
  if (!gcInfo || gcInfo.data.length === 0) {
    throw new Error('GlobalConfig missing — run `deploy.ts` first');
  }
  const gc = await GlobalConfig.load({ connection });
  if (!gc) throw new Error('GlobalConfig decoded as null — run `deploy.ts` first');
  if (!gc.protocolAdmin().equals(payer.publicKey)) {
    throw new Error(
      `payer ${fmtKey(payer.publicKey)} is not GlobalConfig.protocol_admin ` +
        `(${fmtKey(gc.protocolAdmin())}); re-run with --payer <protocol-admin-kp>`,
    );
  }

  if (!skipCreateMarket) {
    header(`Sending CreateMarket [${label}]`);
    const rent = await connection.getMinimumBalanceForRentExemption(MARKET_FIXED_SIZE);
    const allocIx = SystemProgram.createAccount({
      fromPubkey: payer.publicKey,
      newAccountPubkey: marketKeypair.publicKey,
      lamports: rent,
      space: MARKET_FIXED_SIZE,
      programId: YDELTA_PROGRAM_ID,
    });
    const createIx = createCreateMarketInstruction({
      payer: payer.publicKey,
      market: marketKeypair.publicKey,
      debtMint,
      collateralMint,
      marginfiGroup,
      debtBank,
      collateralBank,
      marginfiProgram,
    });
    createMarketSig = await sendIxs(
      connection,
      payer,
      [allocIx, createIx],
      [marketKeypair],
      { finalize: true },
    );
    console.log(`  tx: ${createMarketSig}`);
    console.log(`  ⚠  market is paused by default — send SetMarketPause(false) when ready.`);
  }

  const market = await Market.loadFromAddress({ connection, address: marketKeypair.publicKey });
  console.log(`  ✓ Market live; admin=${fmtKey(market.admin())}`);

  // ─── Step 2: CreateVault (debt-side; idempotent) ──────────────────────
  let createVaultSig: string | undefined;
  const [vault] = globalVaultPda(debtMint);
  if (skipVault) {
    console.log(`  · skipVault set; not creating GlobalVault for ${fmtKey(debtMint)}`);
  } else {
    const existing = await connection.getAccountInfo(vault);
    if (existing && existing.data.length > 0) {
      console.log(`  ✓ GlobalVault for debt-mint already exists at ${fmtKey(vault)}`);
    } else {
      header(`Sending CreateVault (debt-mint = ${fmtKey(debtMint)})`);
      const ix = createCreateVaultInstruction({
        payer: payer.publicKey,
        mint: debtMint,
        marginfiGroup,
        lendingPool: debtBank,
        marginfiProgram,
        tokenProgram: TOKEN_PROGRAM_ID,
        tokenProgram22: TOKEN_2022_PROGRAM_ID,
      });
      createVaultSig = await sendIxs(connection, payer, [ix], [], { finalize: true });
      console.log(`  tx: ${createVaultSig}`);
      const v = await Vault.loadForMint({ connection, mint: debtMint });
      if (!v) throw new Error('Vault not visible after CreateVault — re-run');
      console.log(`  ✓ Vault live; admin=${fmtKey(v.admin())}, lending pool=${fmtKey(v.lendingPool())}`);
    }
  }

  // ─── Step 3: optional unpause + assemble output ───────────────────────
  let unpauseSig: string | undefined;
  if (spec.unpauseAfterSetup === true) {
    header(`Sending SetMarketPause(false) [${label}]`);
    const unpauseIx = createSetMarketPauseInstruction(
      { admin: payer.publicKey, market: marketKeypair.publicKey },
      { paused: false },
    );
    unpauseSig = await sendIxs(connection, payer, [unpauseIx], [], { finalize: true });
    console.log(`  tx: ${unpauseSig}`);
    console.log(`  ✓ Market unpaused — it is now live.`);
  } else {
    console.log(`\n  ⚠  Market remains PAUSED. After verifying setup, either:`);
    console.log(`     - set unpauseAfterSetup=true in the config and re-run, or`);
    console.log(`     - send SetMarketPause(false) manually from the admin keypair.`);
  }

  const [marketSigner] = marketSignerPda(marketKeypair.publicKey);
  const [debtVaultAddr] = marketVaultPda(marketKeypair.publicKey, debtMint);
  const [collateralVaultAddr] = marketVaultPda(marketKeypair.publicKey, collateralMint);
  const [lenderMfi] = lenderMarginfiAccountPda(marketKeypair.publicKey);
  const [borrowerMfi] = borrowerMarginfiAccountPda(marketKeypair.publicKey);
  const [vaultSigner] = globalVaultSignerPda(vault);
  const [vaultIntegration] = vaultIntegrationAccountPda(vault);
  const [vaultStaging] = vaultStagingPda(vault);

  const output: MarketOutput = {
    cluster,
    rpc: connection.rpcEndpoint,
    name: spec.name,
    market: fmtKey(marketKeypair.publicKey),
    marketSigner: fmtKey(marketSigner),
    protocolAdmin: fmtKey(payer.publicKey),
    debtMint: fmtKey(debtMint),
    collateralMint: fmtKey(collateralMint),
    debtVault: fmtKey(debtVaultAddr),
    collateralVault: fmtKey(collateralVaultAddr),
    lenderMarginfiAccount: fmtKey(lenderMfi),
    borrowerMarginfiAccount: fmtKey(borrowerMfi),
    marginfiGroup: fmtKey(marginfiGroup),
    marginfiProgram: fmtKey(marginfiProgram),
    debtBank: fmtKey(debtBank),
    collateralBank: fmtKey(collateralBank),
    globalVault: {
      address: fmtKey(vault),
      signer: fmtKey(vaultSigner),
      integrationAccount: fmtKey(vaultIntegration),
      staging: fmtKey(vaultStaging),
    },
    createdAtUnix: Math.floor(Date.now() / 1000),
    txSignatures: {
      createMarket: createMarketSig,
      ...(createVaultSig ? { createVault: createVaultSig } : {}),
      ...(unpauseSig ? { unpause: unpauseSig } : {}),
    },
  };

  const outPath =
    outPathOverride ??
    path.join(LOCAL_DIR, 'markets', `${fmtKey(marketKeypair.publicKey)}.json`);
  writeJson(outPath, output);

  // Persist the market keypair so subsequent runs (or re-runs) can pin it.
  // Parent dir is 0o700 because it holds raw secret keys.
  const kpPath = path.join(LOCAL_DIR, 'markets', `${fmtKey(marketKeypair.publicKey)}.keypair.json`);
  mkdirp(kpPath, { mode: 0o700 });
  fs.writeFileSync(
    kpPath,
    JSON.stringify(Array.from(marketKeypair.secretKey), null, 2) + '\n',
    { mode: 0o600 },
  );
  console.log(`  wrote ${path.relative(process.cwd(), kpPath)} (mode 0600)`);
  console.log(`  → output: ${path.relative(process.cwd(), outPath)}`);

  return output;
}

async function main(): Promise<void> {
  loadDotEnv();
  const args = parseArgs(process.argv.slice(2));
  const cluster = optionalArg(args, 'cluster') ?? defaultCluster();
  if (!cluster) throw new Error('missing required --cluster (or YDELTA_CLUSTER in .env)');
  const payer = loadKeypair(optionalArg(args, 'payer') ?? defaultPayerPath());
  const connection = makeConnection(cluster);
  assertNotDevnet(cluster, connection.rpcEndpoint);
  await confirmMainnetIfNeeded(connection, cluster, args);

  const configPath = optionalArg(args, 'config');
  const specs: MarketSpec[] = configPath
    ? loadMarketsConfig(configPath).markets
    : [specFromFlags(args)];

  if (configPath) {
    header(`Batch init-market: ${specs.length} market(s) from ${configPath}`);
  }

  // `--out` only meaningful in single-market mode; in batch mode each market
  // writes to .local/markets/<market>.json.
  const outPathOverride = specs.length === 1 ? optionalArg(args, 'out') : undefined;

  // Per-iteration loop. On any failure, stop — successful markets remain
  // on-chain (idempotent), and the operator can fix the bad spec and re-run.
  const outputs: MarketOutput[] = [];
  for (let i = 0; i < specs.length; i++) {
    const spec = specs[i];
    try {
      const out = await initOneMarket(connection, payer, cluster, spec, outPathOverride);
      outputs.push(out);
    } catch (err) {
      console.error(`\n✗ market[${i}]${spec.name ? ` "${spec.name}"` : ''} failed:`);
      console.error((err as Error).stack ?? (err as Error).message);
      console.error(`\n  ${outputs.length}/${specs.length} markets succeeded before this failure.`);
      throw err;
    }
  }

  header(`Done — ${outputs.length} market(s)`);
  for (const o of outputs) {
    const tag = o.name ? o.name.padEnd(20) : 'market'.padEnd(20);
    console.log(`  ${tag} ${o.market}  vault=${o.globalVault.address}`);
  }
}

main().catch((err) => {
  console.error('\nINIT-MARKET FAILED:');
  console.error((err as Error).stack ?? (err as Error).message);
  process.exit(1);
});
