/** Shank-flavoured IDL JSON shape (Solita / Codama compatible). */

export type IdlPrimitive =
  | 'bool'
  | 'u8'
  | 'i8'
  | 'u16'
  | 'i16'
  | 'u32'
  | 'i32'
  | 'u64'
  | 'i64'
  | 'u128'
  | 'i128'
  | 'publicKey'
  | 'string'
  | 'bytes';

export type IdlType =
  | IdlPrimitive
  | { defined: string }
  | { option: IdlType }
  | { vec: IdlType }
  | { array: [IdlType, number] };

export type IdlAccountMeta = {
  name: string;
  isMut: boolean;
  isSigner: boolean;
  docs?: string[];
};

export type IdlField = {
  name: string;
  type: IdlType;
  docs?: string[];
};

export type IdlInstruction = {
  name: string;
  /** Human-readable tag prose. */
  docs?: string[];
  accounts: IdlAccountMeta[];
  args: IdlField[];
  /** All ydelta instructions use a leading u8 tag (0–41). */
  discriminant: { type: 'u8'; value: number };
};

export type IdlStructType = {
  kind: 'struct';
  fields: IdlField[];
};

export type IdlEnumVariant = {
  name: string;
  /** Variant-specific u8/u16/u32 discriminant when fixed. */
  discriminant?: number;
};

export type IdlEnumType = {
  kind: 'enum';
  variants: IdlEnumVariant[];
};

export type IdlTypeDef = {
  name: string;
  type: IdlStructType | IdlEnumType;
  docs?: string[];
};

/** Pod-account header. `discriminator` is the leading u64 the program checks. */
export type IdlAccount = {
  name: string;
  /** 8-byte u64 (LE) prefix the program stamps + verifies. */
  discriminator: number[];
  type: IdlStructType;
  docs?: string[];
};

export type IdlErrorCode = {
  code: number;
  name: string;
  msg: string;
};

export type IdlMetadata = {
  address: string;
  origin: 'shank' | 'hand-written';
};

export type Idl = {
  version: string;
  name: string;
  instructions: IdlInstruction[];
  accounts: IdlAccount[];
  types: IdlTypeDef[];
  errors: IdlErrorCode[];
  metadata: IdlMetadata;
};
