#!/usr/bin/env -S npx ts-node
/* eslint-disable no-console */
import * as fs from 'fs';
import * as path from 'path';
import { Keypair, PublicKey } from '@solana/web3.js';
import {
  assertNotDevnet,
  confirmMainnetIfNeeded,
  defaultCluster,
  defaultPayerPath,
  encodeKeypairToCliJson,
  fmtKey,
  header,
  LOCAL_DIR,
  loadDotEnv,
  loadKeypair,
  makeConnection,
  mkdirp,
  optionalArg,
  parseArgs,
  readJsonIfExists,
  requireArg,
  safeFileSegment,
  sendIxs,
  writeJson,
} from './common';
import { globalConfigPda, globalVaultPda } from '../../client/ts/src/utils/pdas';
import { createCreateRiskProfileInstruction } from '../../client/ts/src/ydelta/instructions';
import { Vault } from '../../client/ts/src/vault';

/* USAGE:
 *
 *   yarn setup:risk-profiles \
 *     --cluster localhost \
 *     --debt-mint <USDC>
 *
 *   [--payer <path>]
 *   [--out <path>]                              # default: .local/vaults/<DEBT_MINT>-profiles.json
 *   [--curators-out <path>]                     # default: .local/curators/<cluster>/<DEBT_MINT>.json (gitignored)
 *
 * Risk profiles are state on the GlobalVault (which is keyed by debt mint),
 * not on any individual market — so this script operates per-vault. Every
 * market that uses the same debt mint shares these profiles.
 *
 * Generates 5 fresh curator keypairs (one per profile), creates 5 RiskProfiles
 * spanning Conservative → Maximum risk bands, and writes:
 *   - .local/vaults/<DEBT_MINT>-profiles.json   (public — checked-in safe)
 *   - .local/curators/<cluster>/<DEBT_MINT>.json (PRIVATE — keypairs; gitignored)
 *
 * Idempotent: existing profile_ids are skipped and surfaced in the output
 * (without secret keys, since this run didn't mint those curators).
 *
 * Profile bands (sized for stablecoin-debt against blue-chip collateral):
 *
 *   ID  Name         max_ltv   max_term       allowed_markets   target curator
 *   1   Conservative  40%       7 days         2                 risk-averse desks
 *   2   Standard      50%      14 days         3                 generalist
 *   3   Balanced      65%      30 days         4                 yield-focused
 *   4   Aggressive    75%      60 days         5                 risk-tolerant
 *   5   Maximum       85%      90 days         8                 max-yield bidders
 *
 * 85% Maximum brushes marginfi's init-LTV ceiling for SOL-family collateral;
 * the protocol clamps at loan-time so the profile itself is safe to create.
 */

type ProfileConfig = {
  profileId: number;
  name: string;
  description: string;
  maxLtvBps: number;
  maxTermSeconds: number;
  allowedMarketMax: number;
};

const PROFILES: ProfileConfig[] = [
  {
    profileId: 1,
    name: 'Conservative',
    description: 'Risk-averse curator: short terms, low LTV, narrow market exposure.',
    maxLtvBps: 4_000,
    maxTermSeconds: 7 * 86_400,
    allowedMarketMax: 2,
  },
  {
    profileId: 2,
    name: 'Standard',
    description: 'Generalist curator: moderate LTV and term, comfortable across a few markets.',
    maxLtvBps: 5_000,
    maxTermSeconds: 14 * 86_400,
    allowedMarketMax: 3,
  },
  {
    profileId: 3,
    name: 'Balanced',
    description: 'Yield-focused curator: blue-chip collateral, ~30-day terms.',
    maxLtvBps: 6_500,
    maxTermSeconds: 30 * 86_400,
    allowedMarketMax: 4,
  },
  {
    profileId: 4,
    name: 'Aggressive',
    description: 'Risk-tolerant curator: high LTV, long terms, broad market footprint.',
    maxLtvBps: 7_500,
    maxTermSeconds: 60 * 86_400,
    allowedMarketMax: 5,
  },
  {
    profileId: 5,
    name: 'Maximum',
    description: 'Max-yield bidder: brushes the marginfi init-LTV ceiling.',
    maxLtvBps: 8_500,
    maxTermSeconds: 90 * 86_400,
    allowedMarketMax: 8,
  },
];

type ProfileOutput = ProfileConfig & {
  curator: string;
  txSignature: string | null;
};

type CuratorSecret = {
  /** profile_id → array of u8 secret-key bytes (Solana-CLI JSON format). */
  [profileId: string]: {
    pubkey: string;
    profileName: string;
    debtMint: string;
    secretKey: number[];
  };
};

async function main(): Promise<void> {
  loadDotEnv();
  const args = parseArgs(process.argv.slice(2));
  const cluster = optionalArg(args, 'cluster') ?? defaultCluster();
  if (!cluster) throw new Error('missing required --cluster (or YDELTA_CLUSTER in .env)');
  const debtMint = new PublicKey(requireArg(args, 'debt-mint'));
  const payer = loadKeypair(optionalArg(args, 'payer') ?? defaultPayerPath());
  const connection = makeConnection(cluster);
  assertNotDevnet(cluster, connection.rpcEndpoint);
  await confirmMainnetIfNeeded(connection, cluster, args);

  const [vaultAddr] = globalVaultPda(debtMint);

  header(`Create 5 risk profiles on vault for ${fmtKey(debtMint)}`);
  console.log(`  Payer (vault admin): ${fmtKey(payer.publicKey)}`);
  console.log(`  Debt mint:           ${fmtKey(debtMint)}`);
  console.log(`  Vault PDA:           ${fmtKey(vaultAddr)}`);

  // Sanity: confirm payer is the vault's global_vault_admin.
  const vault = await Vault.loadForMint({ connection, mint: debtMint });
  if (!vault) {
    throw new Error(
      `Vault not found for debt mint ${fmtKey(debtMint)} — run \`setup:init-market\` first ` +
        `with a market that uses this debt mint, or pass a different --debt-mint`,
    );
  }
  if (!vault.admin().equals(payer.publicKey)) {
    throw new Error(
      `payer ${fmtKey(payer.publicKey)} is not vault admin (${fmtKey(vault.admin())}); ` +
        `pass --payer <kp> with the matching vault admin key`,
    );
  }
  console.log(`  ✓ payer is vault admin`);

  // Require GlobalConfig to exist before sending CreateRiskProfile ixs.
  const [globalConfigAddr] = globalConfigPda();
  const gcInfo = await connection.getAccountInfo(globalConfigAddr);
  if (!gcInfo || gcInfo.data.length === 0) {
    throw new Error('GlobalConfig missing — run `deploy.ts` first');
  }

  // ─── Iterate the 5 profiles ───────────────────────────────────────────
  const debtMintSegment = safeFileSegment(fmtKey(debtMint));
  const curatorsOutPath =
    optionalArg(args, 'curators-out') ??
    path.join(
      LOCAL_DIR,
      'curators',
      safeFileSegment(cluster),
      `${debtMintSegment}.json`,
    );
  const existingCurators = readJsonIfExists<CuratorSecret>(curatorsOutPath) ?? {};
  const profileOutputs: ProfileOutput[] = [];

  for (const cfg of PROFILES) {
    header(`Profile ${cfg.profileId} — ${cfg.name}`);
    console.log(`  ${cfg.description}`);
    console.log(`  max_ltv=${cfg.maxLtvBps}bps  max_term=${cfg.maxTermSeconds}s  allowed_markets=${cfg.allowedMarketMax}`);

    // Skip if this profile_id already exists in the vault.
    const existing = vault.getRiskProfile(cfg.profileId);
    if (existing) {
      console.log(`  ✓ already exists (curator=${fmtKey(existing.curator)}); skipping`);
      profileOutputs.push({
        ...cfg,
        curator: fmtKey(existing.curator),
        txSignature: null,
      });
      continue;
    }

    const curator = Keypair.generate();
    const ix = createCreateRiskProfileInstruction(
      { payer: payer.publicKey, mint: debtMint },
      {
        profileId: cfg.profileId,
        curator: curator.publicKey,
        maxLtvBps: cfg.maxLtvBps,
        maxTermSeconds: cfg.maxTermSeconds,
        allowedMarketMax: cfg.allowedMarketMax,
      },
    );
    const sig = await sendIxs(connection, payer, [ix], [], { finalize: true });
    console.log(`  ✓ created  curator=${fmtKey(curator.publicKey)}  tx=${sig.slice(0, 16)}…`);

    profileOutputs.push({
      ...cfg,
      curator: fmtKey(curator.publicKey),
      txSignature: sig,
    });
    existingCurators[String(cfg.profileId)] = {
      pubkey: fmtKey(curator.publicKey),
      profileName: cfg.name,
      debtMint: fmtKey(debtMint),
      secretKey: encodeKeypairToCliJson(curator),
    };
  }

  // ─── Dump public output + private curators ────────────────────────────
  await vault.reload(connection);
  console.log(`\n  Vault now has ${vault.header().riskProfileCount} risk profiles`);

  const publicOutPath =
    optionalArg(args, 'out') ??
    path.join(LOCAL_DIR, 'vaults', `${debtMintSegment}-profiles.json`);
  writeJson(publicOutPath, {
    cluster,
    debtMint: fmtKey(debtMint),
    vault: fmtKey(vaultAddr),
    profiles: profileOutputs,
  });
  console.log(`  ✓ wrote ${path.relative(process.cwd(), publicOutPath)}`);

  // Curators file carries private keys — write at mode 0600 inside a 0700 dir.
  mkdirp(curatorsOutPath, { mode: 0o700 });
  fs.writeFileSync(curatorsOutPath, JSON.stringify(existingCurators, null, 2) + '\n', { mode: 0o600 });
  console.log(`  ✓ wrote ${path.relative(process.cwd(), curatorsOutPath)} (mode 0600)`);

  console.log(`\nDone.`);
}

main().catch((err) => {
  console.error('\nCREATE-RISK-PROFILES FAILED:');
  console.error((err as Error).stack ?? (err as Error).message);
  process.exit(1);
});
