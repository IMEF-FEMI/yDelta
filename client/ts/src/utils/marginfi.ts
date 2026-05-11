import { AccountMeta, PublicKey } from '@solana/web3.js';
import { OracleAccounts, oracleMetas } from './oracle';

export type BankWithOracle = {
  bank: PublicKey;
  oracles: OracleAccounts;
};

/** Build the `(bank, oracle...)` sequence that `marginfi.withdraw` / `marginfi.borrow`
 *  expects as `remaining_accounts` for the health check. Banks must be passed in the
 *  same order as their balances appear on the target marginfi account.
 *
 *  Caller is responsible for resolving the balances order. Helper exists to keep
 *  per-instruction account composition declarative. */
export function buildHealthRemaining(
  banks: BankWithOracle[],
): AccountMeta[] {
  const out: AccountMeta[] = [];
  for (const { bank, oracles } of banks) {
    out.push({ pubkey: bank, isWritable: false, isSigner: false });
    out.push(...oracleMetas(oracles));
  }
  return out;
}
