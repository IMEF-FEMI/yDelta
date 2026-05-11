import { IdlAccount, IdlField } from './format';

/** Convert a u64 discriminator (as a JS bigint) into the 8-byte LE array
 *  consumers use to match account-header bytes. The on-chain `discriminator`
 *  field is the first 8 bytes; off-chain consumers check `data[0..8]`. */
function le8(disc: bigint): number[] {
  const buf = Buffer.alloc(8);
  buf.writeBigUInt64LE(disc);
  return Array.from(buf);
}

/** Explicit `[u8; N]` padding field — matches Rust's `_padN: [u8; N]` shape
 *  exactly so consumers can step over alignment gaps without alignment math. */
function pad(name: string, n: number, note?: string): IdlField {
  return {
    name,
    type: { array: ['u8', n] },
    ...(note ? { docs: [note] } : {}),
  };
}

// Account-header discriminator constants. ASCII bytes spell "ydelta**".
const GLOBAL_CONFIG_DISCRIMINANT = 0x79_64_65_6c_74_61_47_63n; // "ydeltaGc"
const LOAN_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_4c_4en; // "ydeltaLN"
const USER_ACCOUNT_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_55_53n; // "ydeltaUS"
const GLOBAL_VAULT_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_56_61n; // "ydeltaVa"
const MARKET_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_4d_4bn; // "ydeltaMK"

export const accounts: IdlAccount[] = [
  // ─── GlobalConfig — 128 bytes ───
  {
    name: 'GlobalConfig',
    docs: ['Singleton; PDA seeds `[b"global_config"]`.'],
    discriminator: le8(GLOBAL_CONFIG_DISCRIMINANT),
    type: {
      kind: 'struct',
      fields: [
        { name: 'discriminator', type: 'u64' },              // 0..8
        { name: 'protocolAdmin', type: 'publicKey' },        // 8..40
        { name: 'pendingProtocolAdmin', type: 'publicKey' }, // 40..72
        { name: 'isPaused', type: 'u8' },                    // 72..73
        pad('_padding', 7),                                  // 73..80
        pad('_reserved', 48, '6 × u64 reserved.'),           // 80..128
      ],
    },
  },

  // ─── MarketFixed — 512 bytes ───
  {
    name: 'MarketFixed',
    docs: ['Fixed 512-byte header; followed by hypertree dynamic region.'],
    discriminator: le8(MARKET_FIXED_DISCRIMINANT),
    type: {
      kind: 'struct',
      fields: [
        { name: 'discriminator', type: 'u64' },                       // 0..8
        { name: 'version', type: 'u8' },                              // 8..9
        { name: 'debtMintDecimals', type: 'u8' },                     // 9..10
        { name: 'collateralMintDecimals', type: 'u8' },               // 10..11
        { name: 'debtVaultBump', type: 'u8' },                        // 11..12
        { name: 'collateralVaultBump', type: 'u8' },                  // 12..13
        pad('_padding1', 3, 'Align next Pubkey to 16.'),              // 13..16
        { name: 'debtMint', type: 'publicKey' },                      // 16..48
        { name: 'collateralMint', type: 'publicKey' },                // 48..80
        { name: 'debtVault', type: 'publicKey' },                     // 80..112
        { name: 'collateralVault', type: 'publicKey' },               // 112..144
        { name: 'accumulatedProtocolFeeShares', type: 'u128' },       // 144..160
        { name: 'orderSequenceNumber', type: 'u64' },                 // 160..168
        { name: 'matchedLoanSequence', type: 'u64' },                 // 168..176
        { name: 'numBytesAllocated', type: 'u32' },                   // 176..180
        { name: 'bidsRootIndex', type: 'u32' },                       // 180..184
        { name: 'bidsBestIndex', type: 'u32' },                       // 184..188
        { name: 'asksRootIndex', type: 'u32' },                       // 188..192
        { name: 'asksBestIndex', type: 'u32' },                       // 192..196
        { name: 'claimedSeatsRootIndex', type: 'u32' },               // 196..200
        { name: 'matchedLoansRootIndex', type: 'u32' },               // 200..204
        { name: 'freeListHeadIndex', type: 'u32' },                   // 204..208
        { name: 'positionCount', type: 'u32' },                       // 208..212
        { name: 'feeConfig', type: { defined: 'FeeConfig' } },        // 212..236 (24)
        pad('_paddingAfterFee', 4, 'Align integration block to 8.'),  // 236..240
        { name: 'lenderIntegrationAccount', type: 'publicKey' },      // 240..272
        { name: 'borrowerIntegrationAccount', type: 'publicKey' },    // 272..304
        { name: 'debtLendingPool', type: 'publicKey' },               // 304..336
        { name: 'collateralLendingPool', type: 'publicKey' },         // 336..368
        { name: 'marginfiGroup', type: 'publicKey' },                 // 368..400
        { name: 'marketSigner', type: 'publicKey' },                  // 400..432
        { name: 'marketSignerBump', type: 'u8' },                     // 432..433
        { name: 'lenderIntegrationAccountBump', type: 'u8' },         // 433..434
        { name: 'borrowerIntegrationAccountBump', type: 'u8' },       // 434..435
        pad('_paddingBumps', 5, 'Align next Pubkey.'),                // 435..440
        { name: 'admin', type: 'publicKey' },                         // 440..472
        { name: 'pendingAdmin', type: 'publicKey' },                  // 472..504
        { name: 'isPaused', type: 'u8' },                             // 504..505
        pad('_paddingPause', 7, 'Tail padding to 512.'),              // 505..512
      ],
    },
  },

  // ─── LoanFixed — 288 bytes ───
  {
    name: 'LoanFixed',
    docs: ['PDA seeds `[b"loan", market, sequence.to_le_bytes()]`.'],
    discriminator: le8(LOAN_FIXED_DISCRIMINANT),
    type: {
      kind: 'struct',
      fields: [
        { name: 'discriminator', type: 'u64' },                          // 0..8
        { name: 'market', type: 'publicKey' },                           // 8..40
        { name: 'createdBy', type: 'publicKey' },                        // 40..72
        { name: 'matchedLoanSequence', type: 'u64' },                    // 72..80
        { name: 'borrowerMarginfiBorrowShares', type: 'u128' },          // 80..96
        { name: 'debtIndexAtLastAccrual', type: 'u128' },                // 96..112
        { name: 'principalDebtAtoms', type: 'u64' },                     // 112..120
        { name: 'outstandingDebtAtoms', type: 'u64' },                   // 120..128
        { name: 'lenderClaimableAtoms', type: 'u64' },                   // 128..136
        { name: 'collateralAtoms', type: 'u64' },                        // 136..144
        { name: 'accumulatedProtocolFeeAtoms', type: 'u64' },            // 144..152
        { name: 'accumulatedCuratorFeeAtoms', type: 'u64' },             // 152..160
        { name: 'startedAtUnix', type: 'i64' },                          // 160..168
        { name: 'maturesAtUnix', type: 'i64' },                          // 168..176
        { name: 'lastAccruedUnix', type: 'i64' },                        // 176..184
        { name: 'lenderSeatIndex', type: 'u32' },                        // 184..188
        { name: 'borrowerSeatIndex', type: 'u32' },                      // 188..192
        { name: 'borrowerRateBps', type: 'u16' },                        // 192..194
        { name: 'lenderRateBps', type: 'u16' },                          // 194..196
        { name: 'state', type: 'u8', docs: ['Encoded `LoanState`.'] },   // 196..197
        { name: 'loanType', type: 'u8', docs: ['Encoded `LoanType`.'] }, // 197..198
        { name: 'flags', type: 'u8' },                                   // 198..199
        { name: 'version', type: 'u8' },                                 // 199..200
        { name: 'bump', type: 'u8' },                                    // 200..201
        { name: 'lenderKind', type: 'u8', docs: ['Encoded `LenderKind`.'] }, // 201..202
        { name: 'lenderProfileId', type: 'u8' },                         // 202..203
        { name: 'hasRestingSecondaryBid', type: 'u8' },                  // 203..204
        { name: 'curatorFeeBpsSnapshot', type: 'u16' },                  // 204..206
        pad('_padding1', 2, 'Align next Pubkey.'),                       // 206..208
        { name: 'lenderGlobalVault', type: 'publicKey' },                // 208..240
        { name: 'principalRetiredAtoms', type: 'u64' },                  // 240..248
        pad('_paddingAlignU128', 8, 'Align next u128 to 16.'),           // 248..256
        { name: 'lenderDebtSharePriceSnapshotFp48', type: 'u128' },      // 256..272
        { name: 'borrowerCollateralSharePriceSnapshotFp48', type: 'u128' }, // 272..288
      ],
    },
  },

  // ─── UserAccountFixed — 128 bytes ───
  {
    name: 'UserAccountFixed',
    docs: ['PDA seeds `[b"user", owner]`; followed by hypertree blocks.'],
    discriminator: le8(USER_ACCOUNT_FIXED_DISCRIMINANT),
    type: {
      kind: 'struct',
      fields: [
        { name: 'discriminator', type: 'u64' },              // 0..8
        { name: 'owner', type: 'publicKey' },                // 8..40
        { name: 'vaultPositionsRootIndex', type: 'u32' },    // 40..44
        { name: 'marketPositionsRootIndex', type: 'u32' },   // 44..48
        { name: 'openLoansRootIndex', type: 'u32' },         // 48..52
        { name: 'freeListHeadIndex', type: 'u32' },          // 52..56
        { name: 'numBytesAllocated', type: 'u32' },          // 56..60
        { name: 'vaultPositionCount', type: 'u16' },         // 60..62
        { name: 'marketPositionCount', type: 'u16' },        // 62..64
        { name: 'openLoanCount', type: 'u32' },              // 64..68
        { name: 'bump', type: 'u8' },                        // 68..69
        { name: 'version', type: 'u8' },                     // 69..70
        pad('_padding', 2, 'Align next u64 to 8.'),          // 70..72
        pad('_reserved', 56, '7 × u64 reserved.'),           // 72..128
      ],
    },
  },

  // ─── GlobalVaultFixed — 320 bytes ───
  {
    name: 'GlobalVaultFixed',
    docs: ['Per-mint global vault. PDA seeds `[b"vault", mint]`.'],
    discriminator: le8(GLOBAL_VAULT_FIXED_DISCRIMINANT),
    type: {
      kind: 'struct',
      fields: [
        { name: 'discriminator', type: 'u64' },              // 0..8
        { name: 'mint', type: 'publicKey' },                 // 8..40
        { name: 'globalVaultAdmin', type: 'publicKey' },     // 40..72
        { name: 'integrationPool', type: 'publicKey' },      // 72..104
        { name: 'integrationAccount', type: 'publicKey' },   // 104..136
        { name: 'globalVaultSigner', type: 'publicKey' },    // 136..168
        { name: 'lendingPool', type: 'publicKey' },          // 168..200
        { name: 'riskProfilesRootIndex', type: 'u32' },      // 200..204
        { name: 'claimedSeatsRootIndex', type: 'u32' },      // 204..208
        { name: 'marketOrdersRootIndex', type: 'u32' },      // 208..212
        { name: 'profileFreeListHeadIndex', type: 'u32' },   // 212..216
        { name: 'nodeFreeListHeadIndex', type: 'u32' },      // 216..220
        { name: 'numBytesAllocated', type: 'u32' },          // 220..224
        { name: 'riskProfileCount', type: 'u8' },            // 224..225
        { name: 'globalVaultSignerBump', type: 'u8' },       // 225..226
        { name: 'version', type: 'u8' },                     // 226..227
        pad('_pad0', 1),                                     // 227..228
        { name: 'claimedSeatCount', type: 'u32' },           // 228..232
        { name: 'openOrderCount', type: 'u32' },             // 232..236
        pad('_pad1', 4, 'Align next Pubkey to 8.'),          // 236..240
        { name: 'pendingGlobalVaultAdmin', type: 'publicKey' }, // 240..272
        pad('_reserved', 48, '6 × u64 reserved.'),           // 272..320
      ],
    },
  },
];
