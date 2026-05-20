/**
 * Tier 1 e2e: protocol-wide admin lifecycle.
 *
 * Flow (sequential):
 *   1. CreateGlobalConfig — initial signer becomes protocol_admin.
 *   2. SetGlobalPause(true) → header.is_paused == 1.
 *   3. SetGlobalPause(false) → header.is_paused == 0.
 *   4. TransferProtocolAdmin → pending_protocol_admin set.
 *   5. AcceptProtocolAdmin (new admin signs) → admin rotated.
 *
 * No marginfi state required — these ixs are pure on-chain bookkeeping.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

const BPF_LOADER_UPGRADEABLE_PROGRAM_ID = new PublicKey(
  'BPFLoaderUpgradeab1e11111111111111111111111',
);

import {
  acceptProtocolAdminInstruction,
  createGlobalConfigInstruction,
  decodeGlobalConfig,
  globalConfigPda,
  setGlobalPauseInstruction,
  transferProtocolAdminInstruction,
  YDELTA_PROGRAM_ID,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';

/** Derive the ProgramData PDA — the BPF Upgradeable Loader's metadata account. */
function programDataPda(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
  )[0];
}

describe('e2e: global config lifecycle', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  const programData = programDataPda(YDELTA_PROGRAM_ID);

  beforeAll(async () => {
    bk = await bootBankrun();
    admin = bk.payer;
  });

  it('CreateGlobalConfig stamps the signer as protocol_admin', async () => {
    const ix = createGlobalConfigInstruction({
      payer: admin.publicKey,
      programData,
    });
    const meta = await bk.send([ix], [admin]);
    expect(meta.logMessages.some((l) => l.includes('Program ' + YDELTA_PROGRAM_ID.toBase58() + ' success'))).toBe(true);

    const acc = await bk.getAccount(globalConfigPda()[0]);
    expect(acc).not.toBeNull();
    const cfg = decodeGlobalConfig(acc!.data);
    expect(cfg.protocolAdmin.equals(admin.publicKey)).toBe(true);
    expect(cfg.pendingProtocolAdmin.equals(PublicKey.default)).toBe(true);
    expect(cfg.isPaused).toBe(false);
  });

  it('SetGlobalPause toggles is_paused', async () => {
    await bk.send(
      [setGlobalPauseInstruction({ admin: admin.publicKey, paused: true })],
      [admin],
    );
    let cfg = decodeGlobalConfig((await bk.getAccount(globalConfigPda()[0]))!.data);
    expect(cfg.isPaused).toBe(true);

    await bk.send(
      [setGlobalPauseInstruction({ admin: admin.publicKey, paused: false })],
      [admin],
    );
    cfg = decodeGlobalConfig((await bk.getAccount(globalConfigPda()[0]))!.data);
    expect(cfg.isPaused).toBe(false);
  });

  it('Two-step admin transfer rotates protocol_admin', async () => {
    const newAdmin = await bk.fundedKeypair();

    // Step 1: initiator sets pending_admin.
    await bk.send(
      [
        transferProtocolAdminInstruction({
          currentAdmin: admin.publicKey,
          newAdmin: newAdmin.publicKey,
        }),
      ],
      [admin],
    );
    let cfg = decodeGlobalConfig((await bk.getAccount(globalConfigPda()[0]))!.data);
    expect(cfg.protocolAdmin.equals(admin.publicKey)).toBe(true);
    expect(cfg.pendingProtocolAdmin.equals(newAdmin.publicKey)).toBe(true);

    // Step 2: new admin accepts. Signs as both fee-payer and pending_admin.
    await bk.send(
      [acceptProtocolAdminInstruction({ pendingAdmin: newAdmin.publicKey })],
      [newAdmin],
    );
    cfg = decodeGlobalConfig((await bk.getAccount(globalConfigPda()[0]))!.data);
    expect(cfg.protocolAdmin.equals(newAdmin.publicKey)).toBe(true);
    expect(cfg.pendingProtocolAdmin.equals(PublicKey.default)).toBe(true);

    // Old admin can no longer pause (sanity-check the rotation took).
    const meta = await bk.client.tryProcessTransaction(
      await (async () => {
        const ix = setGlobalPauseInstruction({ admin: admin.publicKey, paused: true });
        const { Transaction } = await import('@solana/web3.js');
        const tx = new Transaction({
          feePayer: admin.publicKey,
          recentBlockhash: (await bk.client.getLatestBlockhash())![0],
        });
        tx.add(ix);
        tx.sign(admin);
        return tx;
      })(),
    );
    expect(meta.result).not.toBeNull(); // tx errored — old admin lost authority
  });
});
