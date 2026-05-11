import { AccountMeta, PublicKey } from '@solana/web3.js';

/** Discriminant matches marginfi's `OracleSetup` repr — and the variant names
 *  match `idl/shared-types.ts::OracleSetup`. yDelta's on-chain decoder only
 *  supports `PythPushOracle`, `SwitchboardPull`, and `StakedWithPythPush`;
 *  every other setup fails `OracleSetupUnsupported`. */
export enum OracleSetup {
  None = 0,
  PythLegacy = 1,
  SwitchboardV2 = 2,
  PythPushOracle = 3,
  SwitchboardPull = 4,
  StakedWithPythPush = 5,
}

/** How many accounts must follow the bank in the `remaining_accounts` slice. */
export function oracleAccountCount(setup: OracleSetup): number {
  switch (setup) {
    case OracleSetup.PythPushOracle:
    case OracleSetup.SwitchboardPull:
      return 1;
    case OracleSetup.StakedWithPythPush:
      return 3;
    default:
      return 0;
  }
}

export type OracleAccounts = {
  /** 1, 2, or 3 keys depending on `setup`. Order matches the on-chain decoder
   *  in `programs/ydelta/src/protocol/oracles.rs`. */
  keys: PublicKey[];
};

/** Build the `AccountMeta[]` segment for a bank's oracle in instructions that
 *  pass oracles as a contiguous remaining-accounts slice. All oracle accounts
 *  are read-only and non-signer. */
export function oracleMetas(oracles: OracleAccounts): AccountMeta[] {
  return oracles.keys.map((pubkey) => ({
    pubkey,
    isWritable: false,
    isSigner: false,
  }));
}

/** Hard upper bound (per `DEFAULT_ORACLE_MAX_AGE_SECS`). Anything older than
 *  this — at marginfi's bank-config default — is rejected pre-decode. */
export const DEFAULT_ORACLE_MAX_AGE_SECS = 60;

/** Future-clock skew tolerance (per `MAX_FUTURE_SKEW_SECS`). */
export const MAX_FUTURE_SKEW_SECS = 10;
