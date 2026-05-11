export enum YdeltaInstructionTag {
  CreateMarket = 0,
  ClaimSeat = 1,
  Deposit = 2,
  Withdraw = 3,
  PlaceOrder = 4,
  CancelOrder = 5,
  UpdateOrder = 6,
  ProcessMatchedLoan = 7,
  Repay = 8,
  ClaimRepayment = 9,
  SyncMarketPosition = 10,
  CreateVault = 11,
  CreateRiskProfile = 12,
  GlobalVaultDeposit = 13,
  GlobalVaultWithdraw = 14,
  ClaimSeatForRiskProfile = 15,
  PlaceOrderForRiskProfile = 16,
  CancelOrderForRiskProfile = 17,
  UpdateOrderForRiskProfile = 18,
  ClaimCuratorFee = 19,
  SettleMaturedLoan = 20,
  LiquidateLoan = 21,
  SetFeeConfig = 22,
  ProtocolFeeClaim = 23,
  ClaimRepaymentForRiskProfile = 24,
  TransferMarketAdmin = 25,
  AcceptMarketAdmin = 26,
  TransferGlobalVaultAdmin = 27,
  AcceptGlobalVaultAdmin = 28,
  TransferCurator = 29,
  AcceptCurator = 30,
  SetMarketPause = 31,
  CreateGlobalConfig = 32,
  TransferProtocolAdmin = 33,
  AcceptProtocolAdmin = 34,
  SetGlobalPause = 35,
  UpdateRiskProfile = 36,
  ReleaseSeatForRiskProfile = 37,
  SyncMarketSeatsForRiskProfile = 38,
  ConvertP2PoolToFixed = 39,
  CheckLtvLiquidatable = 40,
  CheckMaturityLiquidatable = 41,
}

export const LAST_INSTRUCTION_TAG: number = YdeltaInstructionTag.CheckMaturityLiquidatable;

// Account discriminators — first u64 (LE) of each Pod-backed account.
// ASCII bytes spell "ydelta**".
export const GLOBAL_CONFIG_DISCRIMINANT = 0x79_64_65_6c_74_61_47_63n; // "ydeltaGc"
export const LOAN_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_4c_4en; // "ydeltaLN"
export const USER_ACCOUNT_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_55_53n; // "ydeltaUS"
export const GLOBAL_VAULT_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_56_61n; // "ydeltaVa"
export const MARKET_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_4d_4bn; // "ydeltaMK"

/** Block sizes (header + per-payload) for the on-chain hypertree allocators. */
export const MARKET_FIXED_SIZE = 512;
export const USER_ACCOUNT_FIXED_SIZE = 128;
export const GLOBAL_VAULT_FIXED_SIZE = 320;

export const MARKET_BLOCK_SIZE = 160;
export const USER_ACCOUNT_BLOCK_SIZE = 160;
export const VAULT_NODE_BLOCK_SIZE = 160;
export const RISK_PROFILE_BLOCK_SIZE = 512;

/** RB-tree node header (left/right/parent + color + type + padding). */
export const RBTREE_OVERHEAD_BYTES = 16;
export const FREE_LIST_OVERHEAD = 4;

/** Sentinel for an empty tree-node pointer; DataIndex is u32. */
export const NIL = 0xff_ff_ff_ff;
export const isNotNil = (idx: number): boolean => idx !== NIL;

export function readU64Le(buffer: Buffer, offset: number): bigint {
  return buffer.readBigUInt64LE(offset);
}
