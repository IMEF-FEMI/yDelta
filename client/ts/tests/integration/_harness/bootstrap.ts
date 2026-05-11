import * as fs from 'fs';
import * as path from 'path';
import {
  Keypair,
  PublicKey,
} from '@solana/web3.js';
import {
  start,
  AddedProgram,
  AddedAccount,
  ProgramTestContext,
  BanksClient,
} from 'solana-bankrun';
import { YDELTA_PROGRAM_ID } from '../../../src/utils/programId';
import {
  MARGINFI_PROGRAM_ID,
  YDELTA_TEST_HARNESS_PROGRAM_ID,
  MAINNET_ACCOUNT_FIXTURES,
} from './mainnet';

/** Repo root (3 levels up from this file). */
const REPO_ROOT = path.resolve(__dirname, '..', '..', '..', '..', '..');
const FIXTURES_DIR = path.resolve(__dirname, '..', 'fixtures');

/** Path to the directory `startAnchor` searches for `<name>.so`. By default
 *  it's `target/deploy/` next to the workspace `Anchor.toml` or `Cargo.toml`. */
function resolveBpfDir(): string {
  return process.env.BPF_OUT_DIR ?? path.join(REPO_ROOT, 'target', 'deploy');
}

/** Solana-account JSON wire format (the same shape `solana account` outputs). */
type AccountJson = {
  pubkey: string;
  account: {
    lamports: number;
    data: [string, string]; // [base64Data, "base64"]
    owner: string;
    executable: boolean;
    rentEpoch: number;
    space?: number;
  };
};

function loadAccountFixture(filename: string, expectedPubkey: PublicKey): AddedAccount {
  const fullPath = path.join(FIXTURES_DIR, filename);
  const raw = fs.readFileSync(fullPath, 'utf8');
  const parsed: AccountJson = JSON.parse(raw);
  if (parsed.pubkey !== expectedPubkey.toBase58()) {
    throw new Error(
      `fixture ${filename} pubkey ${parsed.pubkey} != expected ${expectedPubkey.toBase58()}`,
    );
  }
  const [b64] = parsed.account.data;
  return {
    address: expectedPubkey,
    info: {
      lamports: parsed.account.lamports,
      data: Buffer.from(b64, 'base64'),
      owner: new PublicKey(parsed.account.owner),
      executable: parsed.account.executable,
      rentEpoch: parsed.account.rentEpoch,
    },
  };
}

export type Harness = {
  context: ProgramTestContext;
  banksClient: BanksClient;
  payer: Keypair;
};

export type BootstrapOpts = {
  /** Skip loading the mainnet marginfi state (group / banks / mints / oracles).
   *  Useful for unit-level integration tests that only need the programs. */
  skipMainnetAccounts?: boolean;
  /** Extra accounts to seed into the bankrun ledger. */
  extraAccounts?: AddedAccount[];
};

/** Spin up a bankrun context with ydelta + marginfi + test-harness loaded
 *  and (optionally) the mainnet marginfi state replayed in. */
export async function bootstrap(opts: BootstrapOpts = {}): Promise<Harness> {
  const bpfDir = resolveBpfDir();
  // `startAnchor` expects programs at `<bpfDir>/<name>.so`. We always pass
  // the absolute path to that directory via the env so we don't depend on
  // CWD when tests run.
  process.env.BPF_OUT_DIR = bpfDir;

  const programs: AddedProgram[] = [
    { name: 'ydelta', programId: YDELTA_PROGRAM_ID },
    { name: 'marginfi', programId: MARGINFI_PROGRAM_ID },
    { name: 'ydelta_test_harness', programId: YDELTA_TEST_HARNESS_PROGRAM_ID },
  ];

  const accounts: AddedAccount[] = [];
  if (!opts.skipMainnetAccounts) {
    for (const { filename, pubkey } of MAINNET_ACCOUNT_FIXTURES) {
      accounts.push(loadAccountFixture(filename, pubkey));
    }
  }
  if (opts.extraAccounts) accounts.push(...opts.extraAccounts);

  void REPO_ROOT;
  const context = await start(programs, accounts);
  return {
    context,
    banksClient: context.banksClient,
    payer: context.payer,
  };
}
