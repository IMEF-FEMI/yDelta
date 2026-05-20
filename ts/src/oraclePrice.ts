// Oracle price decoders → fp48 USD-per-whole-token.
import { OracleSetup } from './marginfi.js';

const FP48_SHIFT = 48n;

// PriceUpdateV2 byte map (post-anchor):
//   @8    write_authority_or_tag    32 bytes
//   @40   verification_level        u8  (must be 1 = Full)
//   @41   feed_id                   32 bytes
//   @73   price                     i64
//   @81   conf                      u64
//   @89   exponent                  i32
//   @93   publish_time              i64
const PYTH_PRICE_OFFSET = 73;
const PYTH_CONF_OFFSET = 81;
const PYTH_EXPONENT_OFFSET = 89;
const PYTH_PUBLISH_TIME_OFFSET = 93;
const PYTH_VERIFICATION_LEVEL_OFFSET = 40;
const PYTH_VERIFY_TAG_FULL = 1;
const PYTH_MIN_EXPONENT = -12;
const PYTH_MAX_EXPONENT = 2;

// PullFeedAccountData byte map (post-anchor):
//   @8 + 2256   result.value    i128 (fp18 USD)
const SWB_RESULT_VALUE_OFFSET = 2264;
const SWB_PRECISION = 18;

function pow10(exp: number): bigint {
  let acc = 1n;
  for (let i = 0; i < exp; i++) acc *= 10n;
  return acc;
}

function scaleToFp48(value: bigint, exponent: number): bigint {
  if (exponent >= 0) {
    return (value * pow10(exponent)) << FP48_SHIFT;
  }
  return (value << FP48_SHIFT) / pow10(-exponent);
}

export interface PythPushPrice {
  priceFp48: bigint;
  confFp48: bigint;
  publishTime: bigint;
  exponent: number;
}

export function readPythPushPrice(accountData: Uint8Array | Buffer): PythPushPrice {
  if (accountData.byteLength < PYTH_PUBLISH_TIME_OFFSET + 8) {
    throw new RangeError(`readPythPushPrice: account too small (${accountData.byteLength})`);
  }
  const buf = Buffer.isBuffer(accountData) ? accountData : Buffer.from(accountData);

  const verifyTag = buf[PYTH_VERIFICATION_LEVEL_OFFSET];
  if (verifyTag !== PYTH_VERIFY_TAG_FULL) {
    throw new Error(`Pyth verification level ${verifyTag} != Full (1)`);
  }
  const price = buf.readBigInt64LE(PYTH_PRICE_OFFSET);
  const conf = buf.readBigUInt64LE(PYTH_CONF_OFFSET);
  const exponent = buf.readInt32LE(PYTH_EXPONENT_OFFSET);
  const publishTime = buf.readBigInt64LE(PYTH_PUBLISH_TIME_OFFSET);

  if (price <= 0n) throw new Error(`Pyth price non-positive: ${price}`);
  if (exponent < PYTH_MIN_EXPONENT || exponent > PYTH_MAX_EXPONENT) {
    throw new Error(`Pyth exponent ${exponent} out of range [${PYTH_MIN_EXPONENT}, ${PYTH_MAX_EXPONENT}]`);
  }

  return {
    priceFp48: scaleToFp48(price, exponent),
    confFp48: scaleToFp48(conf, exponent),
    publishTime,
    exponent,
  };
}

export interface SwitchboardPullPrice {
  priceFp48: bigint;
  rawValueFp18: bigint;
}

export function readSwitchboardPullPrice(accountData: Uint8Array | Buffer): SwitchboardPullPrice {
  if (accountData.byteLength < SWB_RESULT_VALUE_OFFSET + 16) {
    throw new RangeError(
      `readSwitchboardPullPrice: account too small (${accountData.byteLength})`,
    );
  }
  const buf = Buffer.isBuffer(accountData) ? accountData : Buffer.from(accountData);
  const lo = buf.readBigUInt64LE(SWB_RESULT_VALUE_OFFSET);
  const hi = buf.readBigInt64LE(SWB_RESULT_VALUE_OFFSET + 8);
  const value = (hi << 64n) | lo;

  if (value <= 0n) throw new Error(`Switchboard price non-positive: ${value}`);
  return {
    priceFp48: (value << FP48_SHIFT) / pow10(SWB_PRECISION),
    rawValueFp18: value,
  };
}

export function readOraclePriceFp48(
  setup: OracleSetup,
  accountData: Uint8Array | Buffer,
): bigint {
  switch (setup) {
    case OracleSetup.PythPushOracle:
      return readPythPushPrice(accountData).priceFp48;
    case OracleSetup.SwitchboardPull:
      return readSwitchboardPullPrice(accountData).priceFp48;
    default:
      throw new Error(
        `readOraclePriceFp48: setup ${OracleSetup[setup] ?? setup} not supported here`,
      );
  }
}
