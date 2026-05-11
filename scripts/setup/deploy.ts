#!/usr/bin/env -S npx ts-node
/* eslint-disable no-console */
import { execFileSync } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import {
  assertNotDevnet,
  confirmMainnetIfNeeded,
  defaultCluster,
  defaultPayerPath,
  expandHome,
  fmtKey,
  header,
  LOCAL_DIR,
  loadDotEnv,
  loadKeypair,
  makeConnection,
  optionalArg,
  parseArgs,
  REPO_ROOT,
  requireArg,
  safeFileSegment,
  sendIxs,
  writeJson,
} from './common';
import { YDELTA_PROGRAM_ID } from '../../client/ts/src/utils/programId';
import { globalConfigPda } from '../../client/ts/src/utils/pdas';
import { createCreateGlobalConfigInstruction } from '../../client/ts/src/ydelta/instructions';
import { GlobalConfig } from '../../client/ts/src/globalConfig';

/* USAGE:
 *
 *   yarn setup:deploy --cluster localhost
 *   yarn setup:deploy --cluster mainnet-beta --payer ~/.config/solana/mainnet.json
 *   yarn setup:deploy --cluster <mainnet-rpc-url> --skip-deploy   # only init GlobalConfig
 *   yarn setup:deploy --cluster localhost --skip-init             # only deploy
 *
 *   --so          override path to ydelta.so (default: target/deploy/ydelta.so)
 *   --program-id  override path to ydelta-keypair.json
 *
 * Idempotent: if the program is already deployed at the expected ID, deploy is
 * skipped. If GlobalConfig already exists, init is skipped.
 *
 * Devnet is rejected by design: this protocol is only deployed to localhost
 * (dev/test) or mainnet-beta (production). See `assertNotDevnet` below.
 */

type Deployment = {
  cluster: string;
  rpc: string;
  programId: string;
  globalConfig: string;
  protocolAdmin: string;
  upgradeAuthority: string;
  deployedAtSlot: number;
  deployedAtUnix: number;
  txSignatures: {
    /** Captured from `solana program deploy --output json` stdout. */
    deployProgram?: string;
    createGlobalConfig?: string;
  };
};

/** Read the upgrade authority from the BPFLoaderUpgradeable ProgramData
 *  account. Returns null if the program has no ProgramData (i.e. is not a
 *  BPF-upgradeable program). */
async function readProgramUpgradeAuthority(
  connection: import('@solana/web3.js').Connection,
  programId: import('@solana/web3.js').PublicKey,
): Promise<import('@solana/web3.js').PublicKey | null> {
  const { PublicKey } = await import('@solana/web3.js');
  const programInfo = await connection.getAccountInfo(programId);
  if (!programInfo || programInfo.data.length < 36) return null;
  // BPFLoaderUpgradeable program account layout: [u32 variant=2, Pubkey programdata_address].
  // variant byte is at offset 0..4 little-endian; pubkey is at 4..36.
  const programDataAddr = new PublicKey(programInfo.data.subarray(4, 36));
  const pdInfo = await connection.getAccountInfo(programDataAddr);
  if (!pdInfo || pdInfo.data.length < 45) return null;
  // ProgramData layout: [u32 variant=3, u64 last_upgrade_slot, Option<Pubkey> upgrade_authority, ELF...]
  // Option<Pubkey>: 1 byte tag (1=Some, 0=None) + 32 bytes pubkey.
  // 4 + 8 = 12 bytes before the option tag; tag at offset 12; pubkey at 13..45.
  const tag = pdInfo.data[12];
  if (tag !== 1) return null;
  return new PublicKey(pdInfo.data.subarray(13, 45));
}

async function main(): Promise<void> {
  loadDotEnv();
  const args = parseArgs(process.argv.slice(2));
  const cluster = optionalArg(args, 'cluster') ?? defaultCluster();
  if (!cluster) throw new Error('missing required --cluster (or YDELTA_CLUSTER in .env)');
  const payerPath = optionalArg(args, 'payer') ?? defaultPayerPath();
  // `--upgrade-authority` lets the operator separate program-upgrade authority
  // from the payer/protocol-admin role. On mainnet, sharing them concentrates
  // every privileged action under one hot key; warn and proceed if not split.
  const upgradeAuthorityPath =
    optionalArg(args, 'upgrade-authority') ??
    process.env.YDELTA_UPGRADE_AUTHORITY_KEYPAIR ??
    payerPath;
  const upgradeAuthorityIsShared = upgradeAuthorityPath === payerPath;
  const skipDeploy = args['skip-deploy'] === true;
  const skipInit = args['skip-init'] === true;
  const soPath = optionalArg(args, 'so') ?? path.join(REPO_ROOT, 'target', 'deploy', 'ydelta.so');
  const programKpPath =
    optionalArg(args, 'program-id') ?? path.join(REPO_ROOT, 'target', 'deploy', 'ydelta-keypair.json');

  const payer = loadKeypair(payerPath);
  const upgradeAuthority = loadKeypair(upgradeAuthorityPath);
  const connection = makeConnection(cluster);
  assertNotDevnet(cluster, connection.rpcEndpoint);
  await confirmMainnetIfNeeded(connection, cluster, args);

  header(`Deploy yDelta → ${cluster}`);
  console.log(`  RPC:               ${connection.rpcEndpoint}`);
  console.log(`  Payer:             ${fmtKey(payer.publicKey)}  (${payerPath})`);
  console.log(`  Upgrade authority: ${fmtKey(upgradeAuthority.publicKey)}  (${upgradeAuthorityPath})`);
  if (upgradeAuthorityIsShared) {
    console.warn(
      `  ⚠  upgrade authority == payer. Acceptable for dev; on mainnet, pass\n` +
        `     --upgrade-authority <kp> or set YDELTA_UPGRADE_AUTHORITY_KEYPAIR to split roles.`,
    );
  }
  console.log(`  Program ID:        ${fmtKey(YDELTA_PROGRAM_ID)}`);
  console.log(`  Program SO:        ${path.relative(REPO_ROOT, soPath)}`);

  // ─── Step 1: deploy program (idempotent) ──────────────────────────────
  const existing = await connection.getAccountInfo(YDELTA_PROGRAM_ID);
  let deploySig: string | undefined;
  if (existing && existing.executable) {
    // Verify the on-chain upgrade authority matches what we'd sign with,
    // so we fail loudly here rather than mid-script with a confusing error.
    const onChainAuthority = await readProgramUpgradeAuthority(connection, YDELTA_PROGRAM_ID);
    if (onChainAuthority && !onChainAuthority.equals(upgradeAuthority.publicKey)) {
      throw new Error(
        `program already deployed but on-chain upgrade authority is ${fmtKey(onChainAuthority)}, ` +
          `not the supplied keypair ${fmtKey(upgradeAuthority.publicKey)} — pass the matching keypair`,
      );
    }
    console.log(`  ✓ Program already deployed (executable=${existing.executable})`);
  } else if (skipDeploy) {
    throw new Error('--skip-deploy was set but the program is not deployed at the expected ID');
  } else {
    if (!fs.existsSync(soPath)) {
      throw new Error(`ydelta.so not found at ${soPath} — run \`scripts/build-program.sh\` first`);
    }
    if (!fs.existsSync(programKpPath)) {
      throw new Error(`program keypair not found at ${programKpPath}`);
    }
    header(`Running \`solana program deploy\``);
    // `solana` runs via execFileSync (no shell), so `~` in paths from .env
    // / CLI flags must be expanded by us before being passed in.
    const cmdArgs: string[] = [
      'program',
      'deploy',
      soPath,
      '--program-id',
      expandHome(programKpPath),
      '--keypair',
      expandHome(payerPath),
      '--url',
      connection.rpcEndpoint,
      '--upgrade-authority',
      expandHome(upgradeAuthorityPath),
      '--max-sign-attempts',
      '60',
      '--output',
      'json',
    ];
    console.log(`  $ solana ${cmdArgs.join(' ')}`);
    // Capture stdout so we can persist the deploy tx signature in the
    // Deployment audit trail. stderr remains inherited for live progress.
    const stdout = execFileSync('solana', cmdArgs, { stdio: ['inherit', 'pipe', 'inherit'] }).toString();
    try {
      const parsed = JSON.parse(stdout);
      deploySig = parsed.signature ?? parsed.tx_signature ?? parsed.txSignature;
      if (deploySig) console.log(`  tx: ${deploySig}`);
    } catch {
      console.warn(`  (could not parse solana CLI JSON output; deploy tx signature not captured)`);
    }
  }

  // ─── Step 2: CreateGlobalConfig (idempotent) ──────────────────────────
  let initSig: string | undefined;
  const [globalConfigPdaAddr] = globalConfigPda();
  const gcExisting = await connection.getAccountInfo(globalConfigPdaAddr);
  if (gcExisting && gcExisting.data.length > 0) {
    console.log(`  ✓ GlobalConfig already initialized at ${fmtKey(globalConfigPdaAddr)}`);
  } else if (skipInit) {
    console.log(`  · --skip-init set; not creating GlobalConfig`);
  } else {
    // After a fresh `solana program deploy` the RPC may briefly fail to see
    // the new program at simulation time ("Attempt to load a program that
    // does not exist"). Poll at `finalized` commitment until the program
    // account is visible and executable before sending CreateGlobalConfig.
    if (deploySig) {
      header(`Waiting for program to finalize on RPC`);
      const start = Date.now();
      const timeoutMs = 90_000;
      let visible = false;
      while (Date.now() - start < timeoutMs) {
        const info = await connection.getAccountInfo(YDELTA_PROGRAM_ID, 'finalized');
        if (info && info.executable) {
          visible = true;
          console.log(`  ✓ program visible at finalized commitment (${((Date.now() - start) / 1000).toFixed(1)}s)`);
          break;
        }
        await new Promise((r) => setTimeout(r, 2_000));
      }
      if (!visible) {
        throw new Error(
          `program ${fmtKey(YDELTA_PROGRAM_ID)} not visible at finalized commitment after ${timeoutMs / 1000}s — ` +
            `re-run \`yarn setup:deploy\` (idempotent) once the RPC catches up`,
        );
      }
    }
    header(`Sending CreateGlobalConfig`);
    const ix = createCreateGlobalConfigInstruction({ payer: payer.publicKey });
    initSig = await sendIxs(connection, payer, [ix], [], { finalize: true });
    console.log(`  tx: ${initSig}`);
    console.log(`  ✓ GlobalConfig initialized; payer is now protocol_admin`);
  }

  // ─── Step 3: verify + dump deployment metadata ────────────────────────
  header(`Verifying`);
  const gc = await GlobalConfig.load({ connection });
  if (!gc) {
    if (skipInit) {
      throw new Error('GlobalConfig missing — re-run without --skip-init');
    }
    throw new Error('GlobalConfig still missing after init — re-run');
  }
  console.log(`  protocolAdmin:        ${fmtKey(gc.protocolAdmin())}`);
  console.log(`  pendingProtocolAdmin: ${fmtKey(gc.pendingProtocolAdmin())}`);
  console.log(`  isPaused:             ${gc.isPaused()}`);

  const slot = await connection.getSlot();
  const deployment: Deployment = {
    cluster,
    rpc: connection.rpcEndpoint,
    programId: fmtKey(YDELTA_PROGRAM_ID),
    globalConfig: fmtKey(globalConfigPdaAddr),
    protocolAdmin: fmtKey(gc.protocolAdmin()),
    upgradeAuthority: fmtKey(upgradeAuthority.publicKey),
    deployedAtSlot: slot,
    deployedAtUnix: Math.floor(Date.now() / 1000),
    txSignatures: {
      ...(deploySig ? { deployProgram: deploySig } : {}),
      ...(initSig ? { createGlobalConfig: initSig } : {}),
    },
  };
  writeJson(
    path.join(LOCAL_DIR, 'deployments', `${safeFileSegment(cluster)}.json`),
    deployment,
  );

  header(`Next steps — rotate privileged keys`);
  console.log(
    `  The payer/upgrade-authority/protocol-admin roles currently sit on hot keys.\n` +
      `  Before exposing this deployment to user funds, rotate each to a multisig:\n` +
      `\n` +
      `    1. Program upgrade authority (BPF):\n` +
      `         solana program set-upgrade-authority ${fmtKey(YDELTA_PROGRAM_ID)} \\\n` +
      `           --new-upgrade-authority <multisig.json> --upgrade-authority ${upgradeAuthorityPath}\n` +
      `\n` +
      `    2. Protocol admin (yDelta):\n` +
      `         (a) tx 1: TransferProtocolAdmin{new_admin = <multisig>} signed by ${fmtKey(payer.publicKey)}\n` +
      `         (b) tx 2: AcceptProtocolAdmin signed by the multisig\n` +
      `\n` +
      `  Re-run \`deploy.ts --skip-deploy --skip-init\` afterwards to refresh the .local snapshot.`,
  );

  console.log(`\nDone.`);
}

main().catch((err) => {
  console.error('\nDEPLOY FAILED:');
  console.error((err as Error).message);
  process.exit(1);
});
