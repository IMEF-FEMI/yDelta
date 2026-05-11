import { IdlField, IdlTypeDef } from './format';

/** Explicit `[u8; N]` padding for `#[repr(C)]` alignment gaps. */
function pad(name: string, n: number, note?: string): IdlField {
  return {
    name,
    type: { array: ['u8', n] },
    ...(note ? { docs: [note] } : {}),
  };
}

/** Shared types referenced by `instructions[]` and `accounts[]`. */
export const types: IdlTypeDef[] = [
  // ─── Enums (1-byte discriminant on the wire) ───
  {
    name: 'Side',
    type: {
      kind: 'enum',
      variants: [
        { name: 'Bid', discriminant: 0 },
        { name: 'Ask', discriminant: 1 },
      ],
    },
  },
  {
    name: 'OrderType',
    type: {
      kind: 'enum',
      variants: [
        { name: 'Limit', discriminant: 0 },
        { name: 'ImmediateOrCancel', discriminant: 1 },
        { name: 'PostOnly', discriminant: 2 },
      ],
    },
  },
  {
    name: 'OrderKind',
    type: {
      kind: 'enum',
      variants: [
        { name: 'Primary', discriminant: 0 },
        { name: 'SecondaryLoanSale', discriminant: 1 },
      ],
    },
  },
  {
    name: 'OwnerKind',
    type: {
      kind: 'enum',
      variants: [
        { name: 'User', discriminant: 0 },
        { name: 'Vault', discriminant: 1 },
      ],
    },
  },
  {
    name: 'LoanState',
    docs: ['1 and 2 are reserved for future intermediate states.'],
    type: {
      kind: 'enum',
      variants: [
        { name: 'Active', discriminant: 0 },
        { name: 'Repaid', discriminant: 3 },
      ],
    },
  },
  {
    name: 'LoanType',
    type: {
      kind: 'enum',
      variants: [
        { name: 'Fixed', discriminant: 0 },
        { name: 'P2Pool', discriminant: 1 },
      ],
    },
  },
  {
    name: 'LenderKind',
    type: {
      kind: 'enum',
      variants: [
        { name: 'User', discriminant: 0 },
        { name: 'Vault', discriminant: 1 },
      ],
    },
  },
  {
    name: 'OracleSetup',
    type: {
      kind: 'enum',
      variants: [
        { name: 'None', discriminant: 0 },
        { name: 'PythLegacy', discriminant: 1 },
        { name: 'SwitchboardV2', discriminant: 2 },
        { name: 'PythPushOracle', discriminant: 3 },
        { name: 'SwitchboardPull', discriminant: 4 },
        { name: 'StakedWithPythPush', discriminant: 5 },
      ],
    },
  },

  // ─── Pod structs (#[repr(C)] — used inside Pod accounts) ───
  // FeeConfig — 24 bytes
  {
    name: 'FeeConfig',
    docs: ['Per-market fee policy. Embedded in MarketFixed at offset 212.'],
    type: {
      kind: 'struct',
      fields: [
        { name: 'protocolFeeBpsFloor', type: 'u16' },        // 0..2
        { name: 'originationBps', type: 'u16' },             // 2..4
        { name: 'curatorSplitBps', type: 'u16' },            // 4..6
        { name: 'curatorFeeBps', type: 'u16' },              // 6..8
        { name: 'liquidationKeeperBps', type: 'u16' },       // 8..10
        { name: 'liquidationProtocolBps', type: 'u16' },     // 10..12
        { name: 'ltvBufferBps', type: 'u16' },               // 12..14
        pad('_paddingForU32', 2, 'Align grace_period_seconds to 4.'), // 14..16
        { name: 'gracePeriodSeconds', type: 'u32' },         // 16..20
        pad('_paddingTail', 4, 'Tail padding to 24.'),       // 20..24
      ],
    },
  },

  // ─── Borsh-encoded instruction args (NO padding — tightly packed) ───
  {
    name: 'DepositParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'amountAtoms', type: 'u64' },
        { name: 'traderIndexHint', type: { option: 'u32' } },
      ],
    },
  },
  {
    name: 'WithdrawParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'amountAtoms', type: 'u64' },
        { name: 'traderIndexHint', type: { option: 'u32' } },
      ],
    },
  },
  {
    name: 'PlaceOrderParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'seatIndexHint', type: { option: 'u32' } },
        { name: 'side', type: 'u8', docs: ['Encoded `Side`.'] },
        { name: 'orderType', type: 'u8', docs: ['Encoded `OrderType`.'] },
        { name: 'flags', type: 'u8' },
        { name: 'kind', type: 'u8', docs: ['Encoded `OrderKind`.'] },
        { name: 'rateBps', type: 'u16' },
        { name: 'termSeconds', type: 'u32' },
        { name: 'principalAtoms', type: 'u64' },
        { name: 'collateralAtoms', type: 'u64' },
        { name: 'askingPriceAtoms', type: 'u64' },
        { name: 'lastValidUnixTs', type: 'i64' },
        { name: 'borrowerLtvBps', type: { option: 'u16' } },
      ],
    },
  },
  {
    name: 'CancelOrderParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'orderSequenceNumber', type: 'u64' },
        { name: 'orderIndexHint', type: { option: 'u32' } },
        { name: 'seatIndexHint', type: { option: 'u32' } },
      ],
    },
  },
  {
    name: 'UpdateOrderParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'orderSequenceNumber', type: 'u64' },
        { name: 'orderIndexHint', type: { option: 'u32' } },
        { name: 'seatIndexHint', type: { option: 'u32' } },
        { name: 'newRateBps', type: { option: 'u16' } },
        { name: 'newTermSeconds', type: { option: 'u32' } },
        { name: 'newPrincipalAtoms', type: { option: 'u64' } },
        { name: 'newCollateralAtoms', type: { option: 'u64' } },
        { name: 'newLastValidUnixTs', type: { option: 'i64' } },
        { name: 'newFlags', type: { option: 'u8' } },
        { name: 'newAskingPriceAtoms', type: { option: 'u64' } },
      ],
    },
  },
  {
    name: 'RepayParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'repayAtoms', type: 'u64' },
        { name: 'fullRepay', type: 'bool' },
        { name: 'borrowerSeatIndexHint', type: { option: 'u32' } },
      ],
    },
  },
  {
    name: 'ClaimRepaymentParams',
    type: {
      kind: 'struct',
      fields: [{ name: 'lenderSeatIndexHint', type: { option: 'u32' } }],
    },
  },
  {
    name: 'ProcessMatchedLoanParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'matchedLoanSequence', type: 'u64' },
        { name: 'matchedLoanIndexHint', type: { option: 'u32' } },
      ],
    },
  },
  {
    name: 'ClaimSeatForRiskProfileParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'profileId', type: 'u8' },
        { name: 'maxExposureAtoms', type: 'u64' },
      ],
    },
  },
  {
    name: 'PlaceOrderForRiskProfileParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'profileId', type: 'u8' },
        { name: 'rateBps', type: 'u16' },
        { name: 'termSeconds', type: 'u32' },
        { name: 'flags', type: 'u8' },
      ],
    },
  },
  {
    name: 'CancelOrderForRiskProfileParams',
    type: { kind: 'struct', fields: [{ name: 'profileId', type: 'u8' }] },
  },
  {
    name: 'UpdateOrderForRiskProfileParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'profileId', type: 'u8' },
        { name: 'newRateBps', type: 'u16' },
        { name: 'newTermSeconds', type: 'u32' },
        { name: 'newFlags', type: 'u8' },
      ],
    },
  },
  {
    name: 'ReleaseSeatForRiskProfileParams',
    type: { kind: 'struct', fields: [{ name: 'profileId', type: 'u8' }] },
  },
  {
    name: 'ConvertP2PoolToFixedParams',
    type: { kind: 'struct', fields: [{ name: 'maxAcceptableRateBps', type: 'u16' }] },
  },
  {
    name: 'CreateRiskProfileParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'profileId', type: 'u8' },
        { name: 'curator', type: 'publicKey' },
        { name: 'maxLtvBps', type: 'u16' },
        { name: 'maxTermSeconds', type: 'u32' },
        { name: 'allowedMarketMax', type: 'u8' },
      ],
    },
  },
  {
    name: 'SetFeeConfigParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'protocolFeeBpsFloor', type: { option: 'u16' } },
        { name: 'originationBps', type: { option: 'u16' } },
        { name: 'curatorSplitBps', type: { option: 'u16' } },
        { name: 'curatorFeeBps', type: { option: 'u16' } },
        { name: 'liquidationKeeperBps', type: { option: 'u16' } },
        { name: 'liquidationProtocolBps', type: { option: 'u16' } },
        { name: 'ltvBufferBps', type: { option: 'u16' } },
        { name: 'gracePeriodSeconds', type: { option: 'u32' } },
      ],
    },
  },
  {
    name: 'SetMarketPauseParams',
    type: { kind: 'struct', fields: [{ name: 'paused', type: 'u8' }] },
  },
  {
    name: 'SetGlobalPauseParams',
    type: { kind: 'struct', fields: [{ name: 'paused', type: 'u8' }] },
  },
  {
    name: 'ClaimCuratorFeeParams',
    type: { kind: 'struct', fields: [{ name: 'profileId', type: 'u8' }] },
  },
  {
    name: 'GlobalVaultDepositParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'amountAtoms', type: 'u64' },
        { name: 'profileId', type: 'u8' },
      ],
    },
  },
  {
    name: 'GlobalVaultWithdrawParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'sharesToBurn', type: 'u128' },
        { name: 'profileId', type: 'u8' },
      ],
    },
  },
  {
    name: 'SyncMarketSeatsForRiskProfileParams',
    type: { kind: 'struct', fields: [{ name: 'profileId', type: 'u8' }] },
  },
  {
    name: 'UpdateRiskProfileParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'profileId', type: 'u8' },
        { name: 'newMaxLtvBps', type: { option: 'u16' } },
        { name: 'newMaxTermSeconds', type: { option: 'u32' } },
        { name: 'newAllowedMarketMax', type: { option: 'u8' } },
      ],
    },
  },
  {
    name: 'TransferAdminParams',
    docs: [
      'Common shape for TransferMarketAdmin / TransferGlobalVaultAdmin /',
      'TransferProtocolAdmin.',
    ],
    type: { kind: 'struct', fields: [{ name: 'newAdmin', type: 'publicKey' }] },
  },
  {
    name: 'TransferCuratorParams',
    type: {
      kind: 'struct',
      fields: [
        { name: 'profileId', type: 'u8' },
        { name: 'newCurator', type: 'publicKey' },
      ],
    },
  },
  {
    name: 'AcceptCuratorParams',
    type: { kind: 'struct', fields: [{ name: 'profileId', type: 'u8' }] },
  },

  // ─── In-tree Pod node types (live inside Market / UserAccount / Vault
  //     hypertree blocks). Every Pod struct is sized 144 bytes (160-byte block
  //     minus 16-byte RB-tree header) except RiskProfile (496) which sits in
  //     the vault's 512-byte profile allocator. ───

  // ClaimedSeat — 144 bytes
  {
    name: 'ClaimedSeat',
    docs: ['Tree-node payload. Composite key: (owner, riskProfileId).'],
    type: {
      kind: 'struct',
      fields: [
        { name: 'owner', type: 'publicKey' },                       // 0..32
        { name: 'debtWithdrawableShares', type: 'u128' },           // 32..48
        { name: 'debtEncumberedShares', type: 'u128' },             // 48..64
        { name: 'collateralWithdrawableShares', type: 'u128' },     // 64..80
        { name: 'collateralEncumberedShares', type: 'u128' },       // 80..96
        { name: 'openBorrowCount', type: 'u32' },                   // 96..100
        { name: 'openLendCount', type: 'u32' },                     // 100..104
        { name: 'ownerKind', type: 'u8' },                          // 104..105
        { name: 'riskProfileId', type: 'u8' },                      // 105..106
        { name: 'riskProfileMaxLtvBps', type: 'u16' },              // 106..108
        pad('_padding', 4),                                         // 108..112
        pad('_reserved', 32, '4 × u64 reserved.'),                  // 112..144
      ],
    },
  },

  // RestingOrder — 144 bytes
  {
    name: 'RestingOrder',
    type: {
      kind: 'struct',
      fields: [
        { name: 'traderSeatIndex', type: 'u32' },                   // 0..4
        { name: 'loanSequenceSnapshot', type: 'u32' },              // 4..8
        { name: 'sequenceNumber', type: 'u64' },                    // 8..16
        { name: 'principalAtoms', type: 'u64' },                    // 16..24
        { name: 'collateralAtoms', type: 'u64' },                   // 24..32
        { name: 'askingPriceAtoms', type: 'u64' },                  // 32..40
        { name: 'lastValidUnixTs', type: 'i64' },                   // 40..48
        { name: 'loanPda', type: 'publicKey' },                     // 48..80
        { name: 'termSeconds', type: 'u32' },                       // 80..84
        { name: 'rateBps', type: 'u16' },                           // 84..86
        { name: 'side', type: 'u8' },                               // 86..87
        { name: 'kind', type: 'u8' },                               // 87..88
        { name: 'orderType', type: 'u8' },                          // 88..89
        { name: 'flags', type: 'u8' },                              // 89..90
        {
          name: 'sharePriceSnapshotBytes',
          type: { array: ['u8', 16] },
          docs: ['fp48 share-price; declared as `[u8;16]` because the field lands at offset 90 (not u128-aligned).'],
        },                                                          // 90..106
        { name: 'borrowerLtvBps', type: 'u16' },                    // 106..108
        pad('_padding2', 4),                                        // 108..112
        pad('_reserved', 32, '4 × u64 reserved.'),                  // 112..144
      ],
    },
  },

  // MatchedLoan — 144 bytes
  {
    name: 'MatchedLoan',
    type: {
      kind: 'struct',
      fields: [
        { name: 'sequence', type: 'u64' },                          // 0..8
        pad('_padding0', 8, 'Align u128 below to 16.'),             // 8..16
        { name: 'borrowerMarginfiBorrowShares', type: 'u128' },     // 16..32
        { name: 'principalAtoms', type: 'u64' },                    // 32..40
        { name: 'originationAtoms', type: 'u64' },                  // 40..48
        { name: 'collateralAtoms', type: 'u64' },                   // 48..56
        { name: 'matchedAtUnix', type: 'i64' },                     // 56..64
        { name: 'lenderSeatIndex', type: 'u32' },                   // 64..68
        { name: 'borrowerSeatIndex', type: 'u32' },                 // 68..72
        { name: 'termSeconds', type: 'u32' },                       // 72..76
        { name: 'borrowerRateBps', type: 'u16' },                   // 76..78
        { name: 'lenderRateBps', type: 'u16' },                     // 78..80
        { name: 'loanType', type: 'u8' },                           // 80..81
        { name: 'flags', type: 'u8' },                              // 81..82
        pad('_padBeforeP5', 6, 'Align referenced_loan_sequence to 8.'), // 82..88
        { name: 'referencedLoanSequence', type: 'u64' },            // 88..96
        { name: 'cashPaidAtoms', type: 'u64' },                     // 96..104
        { name: 'newLenderSeatIndex', type: 'u32' },                // 104..108
        pad('_padding1', 4, 'Align next u128 to 16.'),              // 108..112
        { name: 'lenderDebtSharePriceSnapshotFp48', type: 'u128' }, // 112..128
        { name: 'borrowerCollateralSharePriceSnapshotFp48', type: 'u128' }, // 128..144
      ],
    },
  },

  // VaultPosition — 144 bytes
  {
    name: 'VaultPosition',
    type: {
      kind: 'struct',
      fields: [
        { name: 'vault', type: 'publicKey' },                       // 0..32
        { name: 'profileId', type: 'u8' },                          // 32..33
        pad('_pad0', 15, 'Align shares (u128) to 16.'),             // 33..48
        { name: 'shares', type: 'u128' },                           // 48..64
        { name: 'snapshotSupplyYieldIndexScaled', type: 'u128' },   // 64..80
        { name: 'snapshotDeltaYieldIndexScaled', type: 'u128' },    // 80..96
        { name: 'lastUpdatedUnix', type: 'i64' },                   // 96..104
        pad('_padding', 8),                                         // 104..112
        pad('_reserved', 32, '4 × u64 reserved.'),                  // 112..144
      ],
    },
  },

  // MarketPosition — 144 bytes
  {
    name: 'MarketPosition',
    type: {
      kind: 'struct',
      fields: [
        { name: 'market', type: 'publicKey' },                      // 0..32
        { name: 'seatIndexInMarket', type: 'u32' },                 // 32..36
        pad('_pad0', 12, 'Align u128 to 16.'),                      // 36..48
        { name: 'debtWithdrawableShares', type: 'u128' },           // 48..64
        { name: 'debtEncumberedShares', type: 'u128' },             // 64..80
        { name: 'collateralWithdrawableShares', type: 'u128' },     // 80..96
        { name: 'collateralEncumberedShares', type: 'u128' },       // 96..112
        pad('_reservedPadding', 32, '4 × u64 reserved.'),           // 112..144
      ],
    },
  },

  // UserLoanRef — 144 bytes
  {
    name: 'UserLoanRef',
    type: {
      kind: 'struct',
      fields: [
        { name: 'loan', type: 'publicKey' },                        // 0..32
        { name: 'market', type: 'publicKey' },                      // 32..64
        { name: 'principalAtoms', type: 'u64' },                    // 64..72
        { name: 'startedAtUnix', type: 'i64' },                     // 72..80
        { name: 'maturesAtUnix', type: 'i64' },                     // 80..88
        { name: 'rateBps', type: 'u16' },                           // 88..90
        { name: 'role', type: 'u8' },                               // 90..91
        { name: 'counterpartyKind', type: 'u8' },                   // 91..92
        { name: 'counterpartyProfileId', type: 'u8' },              // 92..93
        pad('_padding', 19),                                        // 93..112
        pad('_reserved', 32, '4 × u64 reserved.'),                  // 112..144
      ],
    },
  },

  // RiskProfile — 496 bytes (lives in vault's 512-byte profile allocator)
  {
    name: 'RiskProfile',
    type: {
      kind: 'struct',
      fields: [
        { name: 'profileId', type: 'u8' },                          // 0..1
        pad('_pad0', 7, 'Align curator (Pubkey) to 8.'),            // 1..8
        { name: 'curator', type: 'publicKey' },                     // 8..40
        { name: 'maxLtvBps', type: 'u16' },                         // 40..42
        pad('_pad1', 2),                                            // 42..44
        { name: 'maxTermSeconds', type: 'u32' },                    // 44..48
        { name: 'allowedMarketCount', type: 'u8' },                 // 48..49
        { name: 'allowedMarketMax', type: 'u8' },                   // 49..50
        pad('_pad2', 14, 'Align u128 below to 16.'),                // 50..64
        { name: 'totalShares', type: 'u128' },                      // 64..80
        { name: 'totalAssetsAtoms', type: 'u64' },                  // 80..88
        { name: 'totalPrincipalAtoms', type: 'u64' },               // 88..96
        { name: 'deployedPrincipalAtoms', type: 'u64' },            // 96..104
        { name: 'encumberedInOrdersAtoms', type: 'u64' },           // 104..112
        { name: 'totalWeightedRateBps', type: 'u128' },             // 112..128
        { name: 'accumulatedCuratorFeeAtoms', type: 'u64' },        // 128..136
        { name: 'lastAccrueUnix', type: 'i64' },                    // 136..144
        { name: 'cumulativeSupplyYieldIndexScaled', type: 'u128' }, // 144..160
        { name: 'cumulativeDeltaYieldIndexScaled', type: 'u128' },  // 160..176
        { name: 'lastSupplyShareValueFp48', type: 'u128' },         // 176..192
        { name: 'pendingCurator', type: 'publicKey' },              // 192..224
        { name: 'activeMarkets', type: { array: ['publicKey', 8] } }, // 224..480
        pad('_reservedA', 16, '2 × u64 reserved.'),                 // 480..496
      ],
    },
  },

  // RiskProfileDepositorSeat — 144 bytes (same shape as VaultPosition)
  {
    name: 'RiskProfileDepositorSeat',
    type: {
      kind: 'struct',
      fields: [
        { name: 'owner', type: 'publicKey' },                       // 0..32
        { name: 'profileId', type: 'u8' },                          // 32..33
        pad('_pad0', 15, 'Align shares to 16.'),                    // 33..48
        { name: 'shares', type: 'u128' },                           // 48..64
        { name: 'snapshotSupplyYieldIndexScaled', type: 'u128' },   // 64..80
        { name: 'snapshotDeltaYieldIndexScaled', type: 'u128' },    // 80..96
        { name: 'lastUpdatedUnix', type: 'i64' },                   // 96..104
        pad('_padding', 8),                                         // 104..112
        pad('_reserved', 32, '4 × u64 reserved.'),                  // 112..144
      ],
    },
  },

  // RiskProfileOrderRef — 144 bytes
  {
    name: 'RiskProfileOrderRef',
    type: {
      kind: 'struct',
      fields: [
        { name: 'market', type: 'publicKey' },                      // 0..32
        { name: 'profileId', type: 'u8' },                          // 32..33
        { name: 'side', type: 'u8' },                               // 33..34
        pad('_pad0', 2),                                            // 34..36
        { name: 'rateBps', type: 'u16' },                           // 36..38
        pad('_pad1', 2),                                            // 38..40
        { name: 'termSeconds', type: 'u32' },                       // 40..44
        pad('_pad2', 4, 'Align order_sequence_in_market to 8.'),    // 44..48
        { name: 'orderSequenceInMarket', type: 'u64' },             // 48..56
        { name: 'placedAtUnix', type: 'i64' },                      // 56..64
        pad('_reserved', 80, '10 × u64 reserved.'),                 // 64..144
      ],
    },
  },
];
