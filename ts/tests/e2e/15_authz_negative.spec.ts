/**
 * Tier 11 e2e: authz negative paths. Every admin-gated ix must reject
 * non-authorised signers with the matching Custom error. These are the
 * "deploy-time bug" tests — a mis-wired authority check is the most
 * common deploy-killer for this kind of protocol.
 *
 *   - SetMarketPause: non-MarketFixed.admin → MarketAdminRequired (43)
 *   - SetFeeConfig: non-MarketFixed.admin → MarketAdminRequired (43)
 *   - SetGlobalPause: non-protocol_admin → ProtocolAdminRequired (48)
 *   - CreatePoolSubVault: non-protocol_admin → ProtocolAdminRequired (48)
 *   - PlaceOrderForSubVault: non-SubVault.curator → VaultCuratorRequired (29)
 *   - CancelOrderForSubVault: non-curator → VaultCuratorRequired (29)
 *   - AcceptMarketAdmin: signer != pending_admin → PendingAdminMismatch (45)
 */
import { beforeAll, describe, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  acceptMarketAdminInstruction,
  cancelOrderForSubVaultInstruction,
  createPoolSubVaultInstruction,
  globalVaultPda,
  placeOrderForSubVaultInstruction,
  setFeeConfigInstruction,
  setGlobalPauseInstruction,
  setMarketPauseInstruction,
  transferMarketAdminInstruction,
  YDELTA_PROGRAM_ID,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import { MARGINFI_GROUP, SOL_BANK, SOL_ORACLE, USDC_BANK, USDC_MINT, USDC_ORACLE } from './_fixtures.ts';
import { expectCustomError, YdeltaError } from './_errors.ts';
import {
  setupGlobalConfig,
  setupMarket,
  setupPoolSubVault,
  setupVault,
} from './_setup.ts';

void globalVaultPda;
void YDELTA_PROGRAM_ID;

describe('e2e: authz negative paths (non-admin/non-curator attempts)', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let curator: Keypair;
  let outsider: Keypair;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);
    curator = await bk.fundedKeypair();
    await setupPoolSubVault(bk, admin, curator.publicKey, { maxLtvBps: 8_000 });
    outsider = await bk.fundedKeypair();
  });

  it('SetMarketPause by non-admin → MarketAdminRequired', async () => {
    await expectCustomError(
      bk.send(
        [setMarketPauseInstruction({ admin: outsider.publicKey, market: market.publicKey, paused: true })],
        [outsider],
      ),
      YdeltaError.MarketAdminRequired,
      'outsider tries to pause market',
    );
  });

  it('SetFeeConfig by non-admin → MarketAdminRequired', async () => {
    await expectCustomError(
      bk.send(
        [
          setFeeConfigInstruction({
            admin: outsider.publicKey,
            market: market.publicKey,
            protocolFeeBpsFloor: 100,
          }),
        ],
        [outsider],
      ),
      YdeltaError.MarketAdminRequired,
      'outsider tries to change fee config',
    );
  });

  it('SetGlobalPause by non-protocol-admin → ProtocolAdminRequired', async () => {
    await expectCustomError(
      bk.send([setGlobalPauseInstruction({ admin: outsider.publicKey, paused: true })], [outsider]),
      YdeltaError.ProtocolAdminRequired,
      'outsider tries to global-pause',
    );
  });

  it('CreatePoolSubVault by non-protocol-admin → ProtocolAdminRequired', async () => {
    await expectCustomError(
      bk.send(
        [
          createPoolSubVaultInstruction({
            payer: outsider.publicKey,
            bank: USDC_BANK,
            curator: outsider.publicKey,
            spreadBps: 0,
            maxLtvBps: 5_000,
            liquidationLtvBps: 6_000,
            maxTermSeconds: 30 * 86_400,
            curatorFeeBps: 0,
          }),
        ],
        [outsider],
      ),
      YdeltaError.ProtocolAdminRequired,
      'outsider tries to allocate a sub-vault',
    );
  });

  it('PlaceOrderForSubVault by non-curator → VaultCuratorRequired', async () => {
    // The split-payer ix needs BOTH signatures, but the curator signature is
    // the one checked against `subVault.curator`. We sign with `outsider` in
    // both slots; the curator gate should reject.
    await expectCustomError(
      bk.send(
        [
          placeOrderForSubVaultInstruction({
            feePayer: outsider.publicKey,
            curator: outsider.publicKey, // not the real curator
            market: market.publicKey,
            debtBank: USDC_BANK,
            marginfiGroup: MARGINFI_GROUP,
            collateralBank: SOL_BANK,
            debtOracles: [USDC_ORACLE],
            collateralOracles: [SOL_ORACLE],
            subVaultId: 1,
          }),
        ],
        [outsider],
      ),
      YdeltaError.VaultCuratorRequired,
      'outsider impersonates curator on PlaceOrder',
    );
  });

  it('CancelOrderForSubVault by non-curator → VaultCuratorRequired', async () => {
    await expectCustomError(
      bk.send(
        [
          cancelOrderForSubVaultInstruction({
            feePayer: outsider.publicKey,
            curator: outsider.publicKey,
            debtBank: USDC_BANK,
            market: market.publicKey,
            subVaultId: 1,
          }),
        ],
        [outsider],
      ),
      YdeltaError.VaultCuratorRequired,
      'outsider impersonates curator on Cancel',
    );
    void curator; // referenced for ESLint; the real curator is set up but not used in this test.
  });

  it('AcceptMarketAdmin by someone who is NOT the pending_admin → PendingAdminMismatch', async () => {
    // Stage a real transfer with `outsider` as pending. Then attempt to
    // accept from a fresh keypair that is NOT the pending admin.
    const pending = await bk.fundedKeypair();
    await bk.send(
      [
        transferMarketAdminInstruction({
          market: market.publicKey,
          currentAdmin: admin.publicKey,
          newAdmin: pending.publicKey,
        }),
      ],
      [admin],
    );

    const wrongAcceptor = await bk.fundedKeypair();
    await expectCustomError(
      bk.send(
        [acceptMarketAdminInstruction({ market: market.publicKey, pendingAdmin: wrongAcceptor.publicKey })],
        [wrongAcceptor],
      ),
      YdeltaError.PendingAdminMismatch,
      'random keypair tries to claim staged admin role',
    );

    // The legit pending admin can still accept — sanity-check the staging
    // wasn't corrupted.
    await bk.send(
      [acceptMarketAdminInstruction({ market: market.publicKey, pendingAdmin: pending.publicKey })],
      [pending],
    );
  });
});
