import { Idl, IdlField, IdlStructType, IdlType } from './format';

/** Byte size of a primitive type. */
function primitiveSize(t: string): number {
  switch (t) {
    case 'bool':
    case 'u8':
    case 'i8':
      return 1;
    case 'u16':
    case 'i16':
      return 2;
    case 'u32':
    case 'i32':
      return 4;
    case 'u64':
    case 'i64':
      return 8;
    case 'u128':
    case 'i128':
      return 16;
    case 'publicKey':
      return 32;
    default:
      throw new Error(`size: unknown primitive '${t}'`);
  }
}

/** Resolve `{defined: 'X'}` against `types[] ∪ accounts[]`. Returns `null`
 *  when X is an enum (enums are 1-byte discriminants on the wire). */
function resolveStructFields(
  idl: Idl,
  name: string,
): { fields: IdlField[] } | { enumTag: true } | null {
  const t = idl.types.find((x) => x.name === name);
  if (t) {
    if (t.type.kind === 'enum') return { enumTag: true };
    return { fields: (t.type as IdlStructType).fields };
  }
  const a = idl.accounts.find((x) => x.name === name);
  if (a) return { fields: a.type.fields };
  return null;
}

/** Byte size of an IDL type, given the IDL for resolving `{defined: ...}`.
 *  Throws on borsh-only constructs (`{option: T}`, `{vec: T}`) — those don't
 *  have a fixed size and shouldn't appear inside Pod structs. */
function typeSize(idl: Idl, t: IdlType): number {
  if (typeof t === 'string') return primitiveSize(t);
  if ('array' in t) {
    const [inner, n] = t.array;
    return typeSize(idl, inner) * n;
  }
  if ('defined' in t) {
    const resolved = resolveStructFields(idl, t.defined);
    if (!resolved) throw new Error(`size: unresolved type '${t.defined}'`);
    if ('enumTag' in resolved) return 1;
    return sumFieldSizes(idl, resolved.fields);
  }
  if ('option' in t) {
    throw new Error(
      `size: '{option: ${JSON.stringify((t as { option: IdlType }).option)}}' is borsh-only; not allowed inside Pod structs`,
    );
  }
  if ('vec' in t) {
    throw new Error(`size: 'vec' is borsh-only; not allowed inside Pod structs`);
  }
  throw new Error(`size: unsupported type ${JSON.stringify(t)}`);
}

function sumFieldSizes(idl: Idl, fields: IdlField[]): number {
  let sum = 0;
  for (const f of fields) sum += typeSize(idl, f.type);
  return sum;
}

/** Sum the byte sizes of all fields in a Pod struct (or Pod account). Used by
 *  `assertPodSizes` to verify the IDL matches canonical struct sizes. */
export function structByteSize(idl: Idl, name: string): number {
  const r = resolveStructFields(idl, name);
  if (!r || 'enumTag' in r) throw new Error(`structByteSize: '${name}' is not a struct`);
  return sumFieldSizes(idl, r.fields);
}

/** Canonical sizes from `programs/ydelta/src/state/constants.rs` +
 *  individual `*::LAYOUT_SIZE` / `const_assert_eq!` invariants. */
export const POD_BYTE_SIZES: ReadonlyArray<{ name: string; bytes: number }> = [
  // Top-level Pod accounts.
  { name: 'GlobalConfig', bytes: 128 },
  { name: 'MarketFixed', bytes: 512 },
  { name: 'LoanFixed', bytes: 288 },
  { name: 'UserAccountFixed', bytes: 128 },
  { name: 'GlobalVaultFixed', bytes: 320 },
  // Pod sub-structs (embedded in accounts or in-tree tree nodes).
  { name: 'FeeConfig', bytes: 24 },
  { name: 'ClaimedSeat', bytes: 144 },
  { name: 'RestingOrder', bytes: 144 },
  { name: 'MatchedLoan', bytes: 144 },
  { name: 'VaultPosition', bytes: 144 },
  { name: 'MarketPosition', bytes: 144 },
  { name: 'UserLoanRef', bytes: 144 },
  { name: 'RiskProfile', bytes: 496 },
  { name: 'RiskProfileDepositorSeat', bytes: 144 },
  { name: 'RiskProfileOrderRef', bytes: 144 },
];

/** Walk every Pod struct/account and assert the field-sum matches the
 *  canonical size. Throws with a precise diff on mismatch. */
export function assertPodSizes(idl: Idl): void {
  const errs: string[] = [];
  for (const { name, bytes } of POD_BYTE_SIZES) {
    let actual: number;
    try {
      actual = structByteSize(idl, name);
    } catch (e) {
      errs.push(`${name}: ${(e as Error).message}`);
      continue;
    }
    if (actual !== bytes) {
      errs.push(`${name}: expected ${bytes} bytes, IDL fields sum to ${actual}`);
    }
  }
  if (errs.length > 0) {
    throw new Error(`Pod size mismatch:\n  ${errs.join('\n  ')}`);
  }
}

/** Pure-data version of the table — useful when consumers want to validate
 *  account-data lengths after a `getAccountInfo` call without taking a TS
 *  dependency on this script. */
export const POD_BYTE_SIZE_TABLE: Readonly<Record<string, number>> = Object.freeze(
  Object.fromEntries(POD_BYTE_SIZES.map((p) => [p.name, p.bytes])),
);
