import { PublicKey } from '@solana/web3.js';
import { ProgramTestContext } from 'solana-bankrun';

/** Bank-account layout offsets (mirrors `programs/marginfi-mocks/src/state.rs`).
 *  - 8 bytes anchor discriminator
 *  - 288 bytes pre-config (mint+vaults+share-values+share-totals+last_update)
 *  - 544 bytes BankConfig
 *  - then trailing fields
 *
 *  Within `BankConfig`: oracle_setup is at offset 313 of the config body. */
const BANK_ANCHOR_HEADER = 8;
const BANK_CONFIG_OFFSET = BANK_ANCHOR_HEADER + 288;
const BC_ORACLE_SETUP = 313;
const BC_ORACLE_MAX_AGE = 504;

export const ORACLE_SETUP_NONE = 0;
export const ORACLE_SETUP_PYTH_LEGACY = 1;
export const ORACLE_SETUP_SWITCHBOARD_V2 = 2;
export const ORACLE_SETUP_PYTH_PUSH = 3;
export const ORACLE_SETUP_SWITCHBOARD_PULL = 4;
export const ORACLE_SETUP_STAKED_WITH_PYTH_PUSH = 5;

/** Patch a bank account in place so its `BankConfig.oracle_setup` byte is set
 *  to `setup`. Mainnet USDC/SOL banks were originally deployed with PythLegacy
 *  (setup = 1); yDelta only supports {PythPush, SwitchboardPull, StakedWithPythPush}.
 *  Flip them to PythPush so the marginfi.health and yDelta oracle adapter
 *  accept the bank in bankrun tests. */
export async function patchBankOracleSetup({
  context,
  bank,
  setup,
  maxAgeSeconds,
}: {
  context: ProgramTestContext;
  bank: PublicKey;
  setup: number;
  /** Optional override of `oracle_max_age`. Pass `0` to take marginfi's default (60s). */
  maxAgeSeconds?: number;
}): Promise<void> {
  const existing = await context.banksClient.getAccount(bank);
  if (!existing) throw new Error(`bank ${bank.toBase58()} not loaded`);
  const buf = Buffer.from(existing.data);
  buf.writeUInt8(setup, BANK_CONFIG_OFFSET + BC_ORACLE_SETUP);
  if (maxAgeSeconds !== undefined) {
    // u16 little-endian — bank's oracle_max_age is stored in seconds.
    buf.writeUInt16LE(maxAgeSeconds, BANK_CONFIG_OFFSET + BC_ORACLE_MAX_AGE);
  }
  context.setAccount(bank, {
    lamports: Number(existing.lamports),
    data: buf,
    owner: existing.owner,
    executable: existing.executable,
    rentEpoch: 0,
  });
}

/** Flip both USDC and SOL banks to PythPush + clear max-age. */
export async function patchUsdcSolBanksToPythPush(context: ProgramTestContext): Promise<void> {
  const { USDC_BANK, SOL_BANK } = await import('./mainnet');
  await patchBankOracleSetup({ context, bank: USDC_BANK, setup: ORACLE_SETUP_PYTH_PUSH });
  await patchBankOracleSetup({ context, bank: SOL_BANK, setup: ORACLE_SETUP_PYTH_PUSH });
}
