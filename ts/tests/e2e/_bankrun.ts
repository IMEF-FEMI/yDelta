/**
 * Bankrun bootstrap for yDelta e2e tests. Mirrors the marginfi-v2 pattern
 * (see `references/marginfi-v2/tests/rootHooks.ts`) but slimmed down: we
 * don't use Anchor, so we drive everything via raw `Transaction` +
 * `BanksClient.processTransaction()` against our SDK's instruction builders.
 *
 * The bootstrap:
 *   1. Loads `ydelta.so` + `marginfi.so` + `ydelta_test_harness.so` from
 *      `target/deploy/`. The marginfi .so is the real mainnet v0.1.8 ELF
 *      dumped to `programs/ydelta/tests/fixtures/marginfi.so`; we copy or
 *      symlink it into the discovery directory.
 *   2. Loads the on-chain fixture JSONs (marginfi group, banks, oracles,
 *      mints) as genesis accounts.
 *   3. Funds a payer keypair so subsequent ixs can pay rent.
 *
 * Subsequent tests reuse one `Bankrun` instance via vitest's `beforeAll`.
 */
import { readFileSync, existsSync, copyFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  SystemProgram,
} from '@solana/web3.js';
import {
  AccountInfoBytes,
  AddedAccount,
  AddedProgram,
  BanksClient,
  BanksTransactionMeta,
  ProgramTestContext,
  start,
} from 'solana-bankrun';

import { YDELTA_PROGRAM_ID } from '../../src/index.js';

// ESM __dirname equivalent.
const __dirname = dirname(fileURLToPath(import.meta.url));
/** Workspace root (`ydelta/`). */
const ROOT = resolve(__dirname, '../../..');

const MARGINFI_PROGRAM_ID = new PublicKey(
  'MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA',
);
const YDELTA_TEST_HARNESS_PROGRAM_ID = new PublicKey(
  // Per `programs/ydelta-test-harness/src/lib.rs::declare_id!`. Hard-code here so
  // changes upstream surface as a load failure rather than silent skew.
  '8u9MzYZUiPyKLdf6QPHZRZHmM8RmiUFNuDuQM7iLWVUv',
);

const BPF_LOADER_UPGRADEABLE_PROGRAM_ID = new PublicKey(
  'BPFLoaderUpgradeab1e11111111111111111111111',
);

/**
 * Bankrun's `start(programs, ...)` writes the program ELF but does NOT create
 * the BPF Upgradeable Loader's `ProgramData` PDA that yDelta's
 * `CreateGlobalConfig` validates against (`process_create_global_config` reads
 * the program-data account's `upgrade_authority` and requires the signer to
 * match). We synthesise the same 45-byte layout the Rust fixture does:
 *
 *   bytes 0..4   : u32 = 3       (ProgramDataAccount enum variant)
 *   bytes 4..12  : slot (zeroed)
 *   byte 12      : 1             (Some<upgrade_authority>)
 *   bytes 13..45 : authority pubkey
 *
 * Owner = `BpfLoaderUpgradeable`; lamports = 1 (rent-exempt-ish for testing).
 */
function synthProgramDataAccount(authority: PublicKey): AddedAccount {
  const programData = PublicKey.findProgramAddressSync(
    [YDELTA_PROGRAM_ID.toBuffer()],
    BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
  )[0];
  const data = Buffer.alloc(45);
  data.writeUInt32LE(3, 0);
  data[12] = 1;
  authority.toBuffer().copy(data, 13);
  return {
    address: programData,
    info: {
      lamports: 1,
      owner: BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
      executable: false,
      rentEpoch: 0,
      data,
    },
  };
}

/** Bankrun discovers .so files from these directories (BPF_OUT_DIR / cwd). */
const SO_SEARCH_DIRS = [
  resolve(ROOT, 'target/deploy'),
  resolve(ROOT, 'programs/ydelta/tests/fixtures'),
];

/**
 * Bankrun looks for `<name>.so` under `BPF_OUT_DIR`. We set it to
 * `target/deploy` and verify the file actually exists; if the marginfi .so
 * is only under `tests/fixtures`, copy it into `target/deploy` so the loader
 * finds it.
 */
function ensureSoFilesDiscoverable(): string {
  const targetDir = resolve(ROOT, 'target/deploy');
  for (const required of ['ydelta.so', 'marginfi.so', 'ydelta_test_harness.so']) {
    const targetPath = resolve(targetDir, required);
    if (existsSync(targetPath)) continue;
    for (const dir of SO_SEARCH_DIRS) {
      const candidate = resolve(dir, required);
      if (existsSync(candidate)) {
        copyFileSync(candidate, targetPath);
        break;
      }
    }
    if (!existsSync(targetPath)) {
      throw new Error(`bankrun bootstrap: cannot find ${required} under ${SO_SEARCH_DIRS.join(' or ')}`);
    }
  }
  process.env.BPF_OUT_DIR = targetDir;
  return targetDir;
}

function loadJsonFixture(filepath: string): AddedAccount {
  const json = JSON.parse(readFileSync(filepath, 'utf-8'));
  return {
    address: new PublicKey(json.pubkey),
    info: {
      lamports: Number(json.account.lamports),
      owner: new PublicKey(json.account.owner),
      executable: json.account.executable,
      rentEpoch: Number(json.account.rentEpoch ?? 0),
      data: Buffer.from(json.account.data[0], json.account.data[1] as BufferEncoding),
    },
  };
}

export interface BankrunHandle {
  context: ProgramTestContext;
  client: BanksClient;
  /** Genesis payer — pre-funded with ~500_000 SOL. */
  payer: Keypair;
  /** Fund a fresh keypair from `payer`. Returns the funded keypair. */
  fundedKeypair(lamports?: number): Promise<Keypair>;
  /** Compose ixs into a v0-style Transaction, sign with the given signers, and process. */
  send(
    ixs: TransactionInstruction[],
    signers: Keypair[],
  ): Promise<BanksTransactionMeta>;
  /** Read an account's bytes via the BanksClient. Returns `null` when absent. */
  getAccount(key: PublicKey): Promise<AccountInfoBytes | null>;
  /**
   * Synthesise an SPL token account at `address` with `amount` atoms of `mint`,
   * owned by `owner`. Bypasses the real mint authority — useful for priming
   * test wallets when the fixture mint is the mainnet one. Mirrors the Rust
   * fixture's `put_token_account`.
   */
  putTokenAccount(args: {
    address: PublicKey;
    mint: PublicKey;
    owner: PublicKey;
    amount: bigint;
  }): Promise<void>;
  /**
   * wSOL-specific synth. Sets `is_native = Some(rent_exempt)` and stamps
   * `lamports = rent_exempt + amount` because SPL Token's native-transfer
   * logic moves lamports alongside the `amount` field. Mirrors the Rust
   * fixture's `put_wsol_token_account`. Use this — NOT `putTokenAccount` —
   * whenever the mint is `So111…112`.
   */
  putWsolTokenAccount(args: {
    address: PublicKey;
    owner: PublicKey;
    amount: bigint;
  }): Promise<void>;
  /**
   * Walk forward the bankrun clock + refresh the loaded Pyth-push (USDC) and
   * Switchboard-pull (SOL) oracle fixtures so their staleness checks pass.
   * Mirrors the Rust fixture's `refresh_oracle_freshness`.
   */
  refreshOracleFreshness(args: {
    pythOracle?: PublicKey;
    switchboardOracle?: PublicKey;
  }): Promise<void>;
  /**
   * Advance the bankrun clock by `seconds`. Used for time-gated ixs like
   * `claim_repayment_for_risk_subVault` (waits until `matures_at_unix`) and
   * `settle_matured_loan` (waits past `matures_at + grace`).
   */
  warpForward(seconds: number | bigint): Promise<void>;
  /**
   * Crash (or pump) a Switchboard-pull oracle by writing a literal i128
   * value into `CurrentResult.value` (struct offset 2256 → byte offset
   * DISC + 2256 = 2264 in the account body). Marginfi v0.1.8 reads this
   * field as an fp18-scaled USD price. Mirrors the Rust fixture's
   * `set_swb_oracle_price_atoms`.
   */
  setSwbOraclePriceAtoms(oracle: PublicKey, fp18Atoms: bigint): Promise<void>;
  /**
   * Read the SPL Token `amount` (u64 LE @ offset 64..72) of `ata`. For wSOL
   * accounts this is the `amount` field — NOT the lamport balance (which
   * includes the rent-exempt reserve). Returns `null` when the account
   * doesn't exist.
   */
  tokenAccountBalance(ata: PublicKey): Promise<bigint | null>;
}

export interface BootOptions {
  /** Include the marginfi group/bank/oracle fixtures as genesis accounts. */
  loadMarginfiFixtures?: boolean;
  /** Additional account fixtures to load (paths relative to repo root). */
  extraFixtures?: string[];
  /**
   * The pubkey to stamp as the yDelta program's upgrade authority in the
   * synth `ProgramData` account. Defaults to bankrun's genesis payer so
   * `CreateGlobalConfig` (signer == upgrade_authority gate) just works.
   * Pass a different key to test rejection paths.
   */
  programUpgradeAuthority?: PublicKey;
}

/**
 * Boot a fresh bankrun context. Reuse with `beforeAll(...)` across a single
 * spec file. Each call is an independent ledger.
 */
export async function bootBankrun(opts: BootOptions = {}): Promise<BankrunHandle> {
  ensureSoFilesDiscoverable();

  const programs: AddedProgram[] = [
    { name: 'ydelta', programId: YDELTA_PROGRAM_ID },
    { name: 'marginfi', programId: MARGINFI_PROGRAM_ID },
    { name: 'ydelta_test_harness', programId: YDELTA_TEST_HARNESS_PROGRAM_ID },
  ];

  const accounts: AddedAccount[] = [];

  // The yDelta ProgramData PDA — synth'd so CreateGlobalConfig's
  // upgrade-authority gate passes. The authority defaults to the bankrun
  // payer; tests can override via `programUpgradeAuthority`.
  // We need the payer pubkey before `start()` resolves to derive this. Use a
  // sentinel address: pre-derive the program-data PDA and stamp it with a
  // placeholder authority that the test will overwrite via `setAccount` after
  // boot if it didn't pass `programUpgradeAuthority`.
  if (opts.programUpgradeAuthority) {
    accounts.push(synthProgramDataAccount(opts.programUpgradeAuthority));
  }

  if (opts.loadMarginfiFixtures) {
    const fx = resolve(ROOT, 'programs/ydelta/tests/fixtures');
    for (const name of [
      'marginfi_group.json',
      'marginfi_usdc_bank.json',
      'marginfi_sol_bank.json',
      'marginfi_usdc_liquidity_vault.json',
      'marginfi_sol_liquidity_vault.json',
      'usdc_mint.json',
      'wsol_mint.json',
      'usdc_oracle.json',
      'sol_oracle.json',
    ]) {
      accounts.push(loadJsonFixture(resolve(fx, name)));
    }
  }
  for (const extra of opts.extraFixtures ?? []) {
    accounts.push(loadJsonFixture(resolve(ROOT, extra)));
  }

  const context = await start(programs, accounts);
  const client = context.banksClient;
  const payer = context.payer;

  // Re-stamp the ProgramData PDA with the actual payer as upgrade authority
  // when the caller didn't pre-supply one. (We can't derive the payer pubkey
  // before `start()` resolves, so this is the lazy path.)
  if (!opts.programUpgradeAuthority) {
    const synth = synthProgramDataAccount(payer.publicKey);
    context.setAccount(synth.address, synth.info);
  }

  return {
    context,
    client,
    payer,

    async fundedKeypair(lamports = 5_000_000_000): Promise<Keypair> {
      const kp = Keypair.generate();
      const blockhash = (await client.getLatestBlockhash())![0];
      const tx = new Transaction({ feePayer: payer.publicKey, recentBlockhash: blockhash });
      tx.add(
        SystemProgram.transfer({
          fromPubkey: payer.publicKey,
          toPubkey: kp.publicKey,
          lamports,
        }),
      );
      tx.sign(payer);
      await client.processTransaction(tx);
      return kp;
    },

    async send(ixs: TransactionInstruction[], signers: Keypair[]): Promise<BanksTransactionMeta> {
      const blockhash = (await client.getLatestBlockhash())![0];
      const tx = new Transaction({
        feePayer: signers[0].publicKey,
        recentBlockhash: blockhash,
      });
      ixs.forEach((ix) => tx.add(ix));
      tx.sign(...signers);
      return client.processTransaction(tx);
    },

    async getAccount(key: PublicKey): Promise<AccountInfoBytes | null> {
      const acc = await client.getAccount(key);
      return acc ?? null;
    },

    async putTokenAccount(args: {
      address: PublicKey;
      mint: PublicKey;
      owner: PublicKey;
      amount: bigint;
    }): Promise<void> {
      const data = Buffer.alloc(165);
      args.mint.toBuffer().copy(data, 0);
      args.owner.toBuffer().copy(data, 32);
      data.writeBigUInt64LE(args.amount, 64);
      // `state` byte at offset 108: 1 = Initialized.
      data[108] = 1;
      const rent = await client.getRent();
      const lamports = Number(rent.minimumBalance(BigInt(165)));
      context.setAccount(args.address, {
        lamports,
        owner: SPL_TOKEN_PROGRAM_ID,
        executable: false,
        rentEpoch: 0,
        data,
      });
    },

    async putWsolTokenAccount(args: {
      address: PublicKey;
      owner: PublicKey;
      amount: bigint;
    }): Promise<void> {
      const data = Buffer.alloc(165);
      WSOL_MINT_PK.toBuffer().copy(data, 0);
      args.owner.toBuffer().copy(data, 32);
      data.writeBigUInt64LE(args.amount, 64);
      data[108] = 1; // state = Initialized
      // is_native: COption<u64> at @109..121 — Some(rent_exempt).
      const rent = await client.getRent();
      const rentExempt = rent.minimumBalance(BigInt(165));
      data.writeUInt32LE(1, 109); // Some tag
      data.writeBigUInt64LE(rentExempt, 113);
      // SPL Token's native-transfer moves lamports alongside `amount`, so
      // the account's lamport balance must equal rent_exempt + amount.
      const lamports = Number(rentExempt + args.amount);
      context.setAccount(args.address, {
        lamports,
        owner: SPL_TOKEN_PROGRAM_ID,
        executable: false,
        rentEpoch: 0,
        data,
      });
    },

    async refreshOracleFreshness(args: {
      pythOracle?: PublicKey;
      switchboardOracle?: PublicKey;
    }): Promise<void> {
      const clock = await client.getClock();
      const ts = clock.unixTimestamp;
      const slot = clock.slot;
      if (args.pythOracle) {
        await rewritePythPostTimestamps(client, context, args.pythOracle, ts);
      }
      if (args.switchboardOracle) {
        await rewriteSwitchboardFreshness(client, context, args.switchboardOracle, ts, slot);
      }
    },

    async warpForward(seconds: number | bigint): Promise<void> {
      const sec = typeof seconds === 'bigint' ? seconds : BigInt(seconds);
      const clock = await client.getClock();
      const { Clock } = await import('solana-bankrun');
      context.setClock(
        new Clock(
          clock.slot,
          clock.epochStartTimestamp,
          clock.epoch,
          clock.leaderScheduleEpoch,
          clock.unixTimestamp + sec,
        ),
      );
    },

    async setSwbOraclePriceAtoms(oracle: PublicKey, fp18Atoms: bigint): Promise<void> {
      const acc = await client.getAccount(oracle);
      if (!acc) throw new Error(`switchboard oracle ${oracle.toBase58()} not loaded`);
      const data = Buffer.from(acc.data);
      // CurrentResult.value at struct offset 2256 → byte offset DISC + 2256 = 2264.
      const VALUE_OFFSET = 2264;
      // i128 LE: low 8 bytes + high 8 bytes (signed).
      const lo = fp18Atoms & 0xffff_ffff_ffff_ffffn;
      const hi = fp18Atoms >> 64n;
      data.writeBigUInt64LE(lo, VALUE_OFFSET);
      data.writeBigInt64LE(hi, VALUE_OFFSET + 8);
      context.setAccount(oracle, { ...acc, data });
    },

    async tokenAccountBalance(ata: PublicKey): Promise<bigint | null> {
      const acc = await client.getAccount(ata);
      if (!acc) return null;
      // SPL Token account: amount is u64 LE at offset 64..72.
      return Buffer.from(acc.data).readBigUInt64LE(64);
    },
  };
}

/** SPL Token classic program id. */
const SPL_TOKEN_PROGRAM_ID = new PublicKey('TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');
const WSOL_MINT_PK = new PublicKey('So11111111111111111111111111111111111111112');

/**
 * Mutate a Pyth-push `PriceUpdateV2` account so its `publish_time` and
 * `prev_publish_time` equal `timestamp`. Body layout post-Anchor-disc:
 *   write_authority(32) + verification_level(1) + price_message{
 *     feed_id(32) + price(8) + conf(8) + exponent(4) +
 *     publish_time(8) + prev_publish_time(8) + …
 *   }
 * Total disc(8) + 32 + 1 + 32 + 8 + 8 + 4 = 93 → publish_time offset.
 */
async function rewritePythPostTimestamps(
  client: BanksClient,
  context: ProgramTestContext,
  address: PublicKey,
  timestamp: bigint,
): Promise<void> {
  const acc = await client.getAccount(address);
  if (!acc) throw new Error(`pyth oracle ${address.toBase58()} not loaded`);
  const data = Buffer.from(acc.data);
  const PUBLISH_TIME_OFFSET = 8 + 32 + 1 + 32 + 8 + 8 + 4; // 93
  const PREV_PUBLISH_TIME_OFFSET = PUBLISH_TIME_OFFSET + 8; // 101
  data.writeBigInt64LE(timestamp, PUBLISH_TIME_OFFSET);
  data.writeBigInt64LE(timestamp, PREV_PUBLISH_TIME_OFFSET);
  context.setAccount(address, { ...acc, data });
}

/**
 * Mutate a Switchboard-pull `PullFeedAccountData` so its
 * `last_update_timestamp` (struct offset 2208) and `result.slot` (struct
 * offset 2360) reflect the current bankrun clock. See the Rust
 * `refresh_swb_pull_oracle` helper for the layout derivation.
 */
async function rewriteSwitchboardFreshness(
  client: BanksClient,
  context: ProgramTestContext,
  address: PublicKey,
  timestamp: bigint,
  slot: bigint,
): Promise<void> {
  const acc = await client.getAccount(address);
  if (!acc) throw new Error(`switchboard oracle ${address.toBase58()} not loaded`);
  const data = Buffer.from(acc.data);
  const DISC = 8;
  const LAST_UPDATE_TIMESTAMP_OFFSET = DISC + 2208;
  const RESULT_SLOT_OFFSET = DISC + 2360;
  data.writeBigInt64LE(timestamp, LAST_UPDATE_TIMESTAMP_OFFSET);
  data.writeBigUInt64LE(slot, RESULT_SLOT_OFFSET);
  context.setAccount(address, { ...acc, data });
}
