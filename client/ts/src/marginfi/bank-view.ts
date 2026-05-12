/**
 * Self-contained marginfi `Bank` raw-bytes decoder.
 *
 * Why this exists in addition to `@mrgnlabs/marginfi-client-v2`'s
 * `Bank.fromBuffer`:
 *
 * - The package's bundled v0.1.7 IDL declares `accounts[].name = "Bank"`
 *   (Anchor 0.30+ format), but its `decodeBankRaw` calls
 *   `BorshAccountsCoder.decode("bank", ...)` via `AccountType.Bank`.
 *   Anchor's coder is case-sensitive — the lookup misses every time.
 * - Even after patching the IDL name case, `parseBankRaw` tries to read
 *   `accountParsed.assetShareValue.value` and hits `undefined` because
 *   the decoded layout doesn't carry `WrappedI80F48` as the expected
 *   sub-struct shape.
 *
 * Rather than monkey-patch around both issues, we decode the bank
 * directly at the offsets pinned in the on-chain
 * `marginfi-mocks::state::Bank` (also used by the Rust cranker in
 * `crankers/src/marginfi_bank.rs`) and produce an object whose field
 * shape matches what marginfi-client-v2's *exported pure helpers*
 * (`computeInterestRates`, `getTotalAssetQuantity`, etc.) consume.
 * Those helpers are reachable from the package root and don't go
 * through the broken `Bank` class.
 *
 * Result: `BankView` is a drop-in for callers that need to compute
 * rates/utilization/quantities, and the marginfi-client-v2 math stays
 * canonical — we just bypass their decoder.
 */

import { Connection, PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import BN from 'bn.js';

import { MARGINFI_PROGRAM_ID } from './client';

/** WrappedI80F48 mantissa lives in the low 16 bytes as `i128 LE` with
 *  an implicit 48-bit fractional. `value / 2^48` → `BigNumber`. */
const I80F48_DENOM = new BigNumber(2).pow(48);

function wrappedI80F48BytesToBn(bytes: Buffer | Uint8Array): BigNumber {
  // Two's-complement i128 LE decode. We never see negatives in healthy
  // banks; if we do, clamp to 0 — same defensive policy the on-chain
  // `wrapped_i80f48_to_u128` follows.
  const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  if (buf.length !== 16) {
    throw new Error(`WrappedI80F48 expected 16 bytes, got ${buf.length}`);
  }
  // High bit of the most-significant byte (LE → buf[15]) signals sign.
  const isNegative = (buf[15] & 0x80) !== 0;
  if (isNegative) {
    return new BigNumber(0);
  }
  // Build positive 128-bit BigNumber from LE bytes.
  let acc = new BigNumber(0);
  let mul = new BigNumber(1);
  const TWO_TO_8 = new BigNumber(256);
  for (let i = 0; i < 16; i++) {
    if (buf[i] !== 0) acc = acc.plus(mul.times(buf[i]));
    mul = mul.times(TWO_TO_8);
  }
  return acc.dividedBy(I80F48_DENOM);
}

// ─────────────────── Bank layout (body offsets) ───────────────────
//
// Field offsets pinned against marginfi v0.1.7 IDL — same values the
// on-chain `marginfi-mocks::state::Bank` slim view uses and the Rust
// cranker reads in `crankers/src/marginfi_bank.rs`.

const BANK_DISC_LEN = 8;

const OFF_MINT = 0;
const OFF_MINT_DECIMALS = 32;
const OFF_GROUP = 33;
const OFF_ASSET_SHARE_VALUE = 72;
const OFF_LIABILITY_SHARE_VALUE = 88;
const OFF_LIQUIDITY_VAULT = 104;
const OFF_LIQUIDITY_VAULT_BUMP = 136;
const OFF_LIQUIDITY_VAULT_AUTH_BUMP = 137;
const OFF_INSURANCE_VAULT = 138;
const OFF_FEE_VAULT = 192;
const OFF_TOTAL_LIABILITY_SHARES = 248;
const OFF_TOTAL_ASSET_SHARES = 264;
const OFF_LAST_UPDATE = 280;
const OFF_BANK_CONFIG = 288;
const OFF_FLAGS = 832;
const OFF_EMISSIONS_RATE = 840;

// BankConfig offsets (relative to body + OFF_BANK_CONFIG = 288).
const BC_OFF_ASSET_WEIGHT_INIT = 0;
const BC_OFF_ASSET_WEIGHT_MAINT = 16;
const BC_OFF_LIABILITY_WEIGHT_INIT = 32;
const BC_OFF_LIABILITY_WEIGHT_MAINT = 48;
const BC_OFF_DEPOSIT_LIMIT = 64;
const BC_OFF_INTEREST_RATE_CONFIG = 72;
const BC_OFF_OPERATIONAL_STATE = 312;
const BC_OFF_ORACLE_SETUP = 313;
const BC_OFF_ORACLE_KEYS = 314;
const BC_OFF_BORROW_LIMIT = 480;
const BC_OFF_RISK_TIER = 488;
const BC_OFF_ASSET_TAG = 489;
const BC_OFF_CONFIG_FLAGS = 490;
const BC_OFF_TOTAL_ASSET_VALUE_INIT_LIMIT = 496;
const BC_OFF_ORACLE_MAX_AGE = 504;
const BC_OFF_ORACLE_MAX_CONFIDENCE = 508;
const BC_OFF_FIXED_PRICE = 512;

// InterestRateConfig offsets (relative to BankConfig + 72).
const IRC_OFF_OPTIMAL_UTILIZATION_RATE = 0;
const IRC_OFF_PLATEAU_INTEREST_RATE = 16;
const IRC_OFF_MAX_INTEREST_RATE = 32;
const IRC_OFF_INSURANCE_FEE_FIXED_APR = 48;
const IRC_OFF_INSURANCE_IR_FEE = 64;
const IRC_OFF_PROTOCOL_FIXED_FEE_APR = 80;
const IRC_OFF_PROTOCOL_IR_FEE = 96;
const IRC_OFF_PROTOCOL_ORIGINATION_FEE = 112;
const IRC_OFF_ZERO_UTIL_RATE = 128;
const IRC_OFF_HUNDRED_UTIL_RATE = 132;
const IRC_OFF_POINTS = 136; // RatePoint[5], 5 × 8 = 40 bytes
const IRC_OFF_CURVE_TYPE = 176;

const MAX_ORACLE_KEYS = 5;

/** A `RatePoint` from `interest_rate_config.points`. `util` and `rate`
 *  are both u32-encoded (`u32 / U32_MAX × X`) — the same encoding the
 *  marginfi-client-v2 multipoint-curve helper expects. */
export type RatePoint = { util: number; rate: number };

/** Shape compatible with marginfi-client-v2's exported pure helpers
 *  (`getTotalAssetQuantity`, `getAssetQuantity`, `computeInterestRates`,
 *  `computeUtilizationRate`, `computeRemainingCapacity`). Field names
 *  / types match what those functions read off `bank` and
 *  `bank.config`. */
export type BankConfigView = {
  assetWeightInit: BigNumber;
  assetWeightMaint: BigNumber;
  liabilityWeightInit: BigNumber;
  liabilityWeightMaint: BigNumber;
  depositLimit: BigNumber;
  borrowLimit: BigNumber;
  operationalState: number;
  oracleSetup: number;
  oracleKeys: PublicKey[];
  oracleMaxAge: number;
  oracleMaxConfidence: number;
  fixedPrice: BigNumber;
  riskTier: number;
  assetTag: number;
  configFlags: number;
  totalAssetValueInitLimit: BigNumber;
  interestRateConfig: {
    optimalUtilizationRate: BigNumber;
    plateauInterestRate: BigNumber;
    maxInterestRate: BigNumber;
    insuranceFeeFixedApr: BigNumber;
    insuranceIrFee: BigNumber;
    protocolFixedFeeApr: BigNumber;
    protocolIrFee: BigNumber;
    protocolOriginationFee: BigNumber;
    zeroUtilRate: number;
    hundredUtilRate: number;
    points: RatePoint[];
    curveType: number;
  };
};

/** Full bank view, suitable for passing into marginfi-client-v2's
 *  exported `compute*` / `get*Quantity` helpers. */
export type BankView = {
  address: PublicKey;
  mint: PublicKey;
  mintDecimals: number;
  group: PublicKey;
  assetShareValue: BigNumber;
  liabilityShareValue: BigNumber;
  liquidityVault: PublicKey;
  liquidityVaultBump: number;
  liquidityVaultAuthorityBump: number;
  insuranceVault: PublicKey;
  feeVault: PublicKey;
  totalLiabilityShares: BigNumber;
  totalAssetShares: BigNumber;
  /** Unix seconds. */
  lastUpdate: number;
  flags: BN;
  emissionsRate: BN;
  config: BankConfigView;
};

/** Decode a `BankView` from raw account data. Skips the 8-byte
 *  Anchor discriminator; caller is expected to have pinned the
 *  account's owner == marginfi program elsewhere. */
export function bankViewFromAccountData(address: PublicKey, data: Buffer | Uint8Array): BankView {
  const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
  if (buf.length < BANK_DISC_LEN + OFF_BANK_CONFIG + 544) {
    throw new Error(
      `bank ${address.toBase58()} account too small (${buf.length} bytes) — expected ≥ 1864`,
    );
  }
  const body = buf.subarray(BANK_DISC_LEN);

  const mint = new PublicKey(body.subarray(OFF_MINT, OFF_MINT + 32));
  const mintDecimals = body[OFF_MINT_DECIMALS];
  const group = new PublicKey(body.subarray(OFF_GROUP, OFF_GROUP + 32));

  const assetShareValue = wrappedI80F48BytesToBn(
    body.subarray(OFF_ASSET_SHARE_VALUE, OFF_ASSET_SHARE_VALUE + 16),
  );
  const liabilityShareValue = wrappedI80F48BytesToBn(
    body.subarray(OFF_LIABILITY_SHARE_VALUE, OFF_LIABILITY_SHARE_VALUE + 16),
  );

  const liquidityVault = new PublicKey(
    body.subarray(OFF_LIQUIDITY_VAULT, OFF_LIQUIDITY_VAULT + 32),
  );
  const liquidityVaultBump = body[OFF_LIQUIDITY_VAULT_BUMP];
  const liquidityVaultAuthorityBump = body[OFF_LIQUIDITY_VAULT_AUTH_BUMP];
  const insuranceVault = new PublicKey(body.subarray(OFF_INSURANCE_VAULT, OFF_INSURANCE_VAULT + 32));
  const feeVault = new PublicKey(body.subarray(OFF_FEE_VAULT, OFF_FEE_VAULT + 32));

  const totalLiabilityShares = wrappedI80F48BytesToBn(
    body.subarray(OFF_TOTAL_LIABILITY_SHARES, OFF_TOTAL_LIABILITY_SHARES + 16),
  );
  const totalAssetShares = wrappedI80F48BytesToBn(
    body.subarray(OFF_TOTAL_ASSET_SHARES, OFF_TOTAL_ASSET_SHARES + 16),
  );
  const lastUpdate = Number(readI64LE(body, OFF_LAST_UPDATE));

  const config = decodeBankConfig(body.subarray(OFF_BANK_CONFIG));

  const flags = new BN(body.subarray(OFF_FLAGS, OFF_FLAGS + 8), 'le');
  const emissionsRate = new BN(body.subarray(OFF_EMISSIONS_RATE, OFF_EMISSIONS_RATE + 8), 'le');

  return {
    address,
    mint,
    mintDecimals,
    group,
    assetShareValue,
    liabilityShareValue,
    liquidityVault,
    liquidityVaultBump,
    liquidityVaultAuthorityBump,
    insuranceVault,
    feeVault,
    totalLiabilityShares,
    totalAssetShares,
    lastUpdate,
    flags,
    emissionsRate,
    config,
  };
}

/** Fetch + decode a bank straight from `getAccountInfo`. Validates the
 *  account is owned by `MARGINFI_PROGRAM_ID`. */
export async function loadBankView(connection: Connection, address: PublicKey): Promise<BankView> {
  const info = await connection.getAccountInfo(address);
  if (!info) {
    throw new Error(`marginfi bank ${address.toBase58()} not found on ${connection.rpcEndpoint}`);
  }
  if (!info.owner.equals(MARGINFI_PROGRAM_ID)) {
    throw new Error(
      `marginfi bank ${address.toBase58()} owned by ${info.owner.toBase58()}, ` +
        `expected ${MARGINFI_PROGRAM_ID.toBase58()}`,
    );
  }
  return bankViewFromAccountData(address, info.data);
}

function decodeBankConfig(cfg: Buffer): BankConfigView {
  const assetWeightInit = wrappedI80F48BytesToBn(cfg.subarray(BC_OFF_ASSET_WEIGHT_INIT, BC_OFF_ASSET_WEIGHT_INIT + 16));
  const assetWeightMaint = wrappedI80F48BytesToBn(cfg.subarray(BC_OFF_ASSET_WEIGHT_MAINT, BC_OFF_ASSET_WEIGHT_MAINT + 16));
  const liabilityWeightInit = wrappedI80F48BytesToBn(cfg.subarray(BC_OFF_LIABILITY_WEIGHT_INIT, BC_OFF_LIABILITY_WEIGHT_INIT + 16));
  const liabilityWeightMaint = wrappedI80F48BytesToBn(cfg.subarray(BC_OFF_LIABILITY_WEIGHT_MAINT, BC_OFF_LIABILITY_WEIGHT_MAINT + 16));
  const depositLimit = new BigNumber(readU64LE(cfg, BC_OFF_DEPOSIT_LIMIT).toString());
  const borrowLimit = new BigNumber(readU64LE(cfg, BC_OFF_BORROW_LIMIT).toString());
  const operationalState = cfg[BC_OFF_OPERATIONAL_STATE];
  const oracleSetup = cfg[BC_OFF_ORACLE_SETUP];
  const oracleKeys: PublicKey[] = [];
  for (let i = 0; i < MAX_ORACLE_KEYS; i++) {
    const off = BC_OFF_ORACLE_KEYS + i * 32;
    const slice = cfg.subarray(off, off + 32);
    if (slice.every((b) => b === 0)) continue;
    oracleKeys.push(new PublicKey(slice));
  }
  const oracleMaxAge = cfg.readUInt16LE(BC_OFF_ORACLE_MAX_AGE);
  const oracleMaxConfidence = cfg.readUInt32LE(BC_OFF_ORACLE_MAX_CONFIDENCE);
  const fixedPrice = wrappedI80F48BytesToBn(cfg.subarray(BC_OFF_FIXED_PRICE, BC_OFF_FIXED_PRICE + 16));
  const riskTier = cfg[BC_OFF_RISK_TIER];
  const assetTag = cfg[BC_OFF_ASSET_TAG];
  const configFlags = cfg[BC_OFF_CONFIG_FLAGS];
  const totalAssetValueInitLimit = new BigNumber(
    readU64LE(cfg, BC_OFF_TOTAL_ASSET_VALUE_INIT_LIMIT).toString(),
  );

  const irc = cfg.subarray(BC_OFF_INTEREST_RATE_CONFIG, BC_OFF_INTEREST_RATE_CONFIG + 240);
  const interestRateConfig = {
    optimalUtilizationRate: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_OPTIMAL_UTILIZATION_RATE, IRC_OFF_OPTIMAL_UTILIZATION_RATE + 16),
    ),
    plateauInterestRate: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_PLATEAU_INTEREST_RATE, IRC_OFF_PLATEAU_INTEREST_RATE + 16),
    ),
    maxInterestRate: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_MAX_INTEREST_RATE, IRC_OFF_MAX_INTEREST_RATE + 16),
    ),
    insuranceFeeFixedApr: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_INSURANCE_FEE_FIXED_APR, IRC_OFF_INSURANCE_FEE_FIXED_APR + 16),
    ),
    insuranceIrFee: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_INSURANCE_IR_FEE, IRC_OFF_INSURANCE_IR_FEE + 16),
    ),
    protocolFixedFeeApr: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_PROTOCOL_FIXED_FEE_APR, IRC_OFF_PROTOCOL_FIXED_FEE_APR + 16),
    ),
    protocolIrFee: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_PROTOCOL_IR_FEE, IRC_OFF_PROTOCOL_IR_FEE + 16),
    ),
    protocolOriginationFee: wrappedI80F48BytesToBn(
      irc.subarray(IRC_OFF_PROTOCOL_ORIGINATION_FEE, IRC_OFF_PROTOCOL_ORIGINATION_FEE + 16),
    ),
    zeroUtilRate: irc.readUInt32LE(IRC_OFF_ZERO_UTIL_RATE),
    hundredUtilRate: irc.readUInt32LE(IRC_OFF_HUNDRED_UTIL_RATE),
    points: decodeRatePoints(irc.subarray(IRC_OFF_POINTS, IRC_OFF_POINTS + 40)),
    curveType: irc[IRC_OFF_CURVE_TYPE],
  };

  return {
    assetWeightInit,
    assetWeightMaint,
    liabilityWeightInit,
    liabilityWeightMaint,
    depositLimit,
    borrowLimit,
    operationalState,
    oracleSetup,
    oracleKeys,
    oracleMaxAge,
    oracleMaxConfidence,
    fixedPrice,
    riskTier,
    assetTag,
    configFlags,
    totalAssetValueInitLimit,
    interestRateConfig,
  };
}

function decodeRatePoints(buf: Buffer): RatePoint[] {
  const out: RatePoint[] = [];
  for (let i = 0; i < 5; i++) {
    const off = i * 8;
    out.push({ util: buf.readUInt32LE(off), rate: buf.readUInt32LE(off + 4) });
  }
  return out;
}

function readU64LE(buf: Buffer, off: number): BN {
  return new BN(buf.subarray(off, off + 8), 'le');
}

function readI64LE(buf: Buffer, off: number): BN {
  // BN with 'le' is unsigned; convert via two's-complement if MSB set.
  const v = new BN(buf.subarray(off, off + 8), 'le');
  // Highest bit at byte off+7. If set, value is negative.
  if (buf[off + 7] & 0x80) {
    return v.fromTwos(64);
  }
  return v;
}
