import { Keypair, PublicKey, SystemProgram } from '@solana/web3.js';
import { ProgramTestContext } from 'solana-bankrun';

/** Generate a deterministic Keypair from a short ASCII label.
 *  Matches marginfi-v2's `setupTestUserBankrun` pattern. */
export function deterministicKeypair(label: string): Keypair {
  const seed = Buffer.alloc(32);
  Buffer.from(label).copy(seed);
  return Keypair.fromSeed(seed);
}

/** Air-drop lamports into a wallet by directly mutating the bankrun account.
 *  Bankrun has no native `requestAirdrop`; setAccount is the canonical path. */
export async function fundLamports(
  context: ProgramTestContext,
  to: PublicKey,
  lamports: number,
): Promise<void> {
  const existing = await context.banksClient.getAccount(to);
  const current = existing
    ? {
        lamports: Number(existing.lamports),
        data: Buffer.from(existing.data),
        owner: existing.owner,
        executable: existing.executable,
      }
    : {
        lamports: 0,
        data: Buffer.alloc(0),
        owner: SystemProgram.programId,
        executable: false,
      };
  context.setAccount(to, {
    ...current,
    rentEpoch: 0,
    lamports: current.lamports + lamports,
  });
}

/** Construct N labelled funded test users. Default each user gets 10 SOL. */
export async function makeUsers(
  context: ProgramTestContext,
  labels: string[],
  lamportsPerUser: number = 10 * 1_000_000_000,
): Promise<Record<string, Keypair>> {
  const out: Record<string, Keypair> = {};
  for (const label of labels) {
    const kp = deterministicKeypair(label);
    await fundLamports(context, kp.publicKey, lamportsPerUser);
    out[label] = kp;
  }
  return out;
}
