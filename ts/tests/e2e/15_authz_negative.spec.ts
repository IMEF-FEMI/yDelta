/**
 * Tier 11 e2e: authz negative paths. Every admin-gated ix must reject
 * non-authorised signers with the matching Custom error. These are the
 * "deploy-time bug" tests — a mis-wired authority check is the most
 * common deploy-killer for this kind of protocol.
 *
 *   - SetMarketPause: non-MarketFixed.admin → MarketAdminRequired (43)
 *   - SetFeeConfig: non-MarketFixed.admin → MarketAdminRequired (43)
 *   - SetGlobalPause: non-protocol_admin → ProtocolAdminRequired (48)
 *   - CreateRiskProfile: non-vault.global_vault_admin → VaultAdminRequired (30)
 *   - PlaceOrderForRiskProfile: non-RiskProfile.curator → VaultCuratorRequired (29)
 *   - CancelOrderForRiskProfile: non-curator → VaultCuratorRequired (29)
 *   - AcceptMarketAdmin: signer != pending_admin → PendingAdminMismatch (45)
 */
import { beforeAll, describe, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  acceptMarketAdminInstruction,
  cancelOrderForRiskProfileInstruction,
  createRiskProfileInstruction,
  globalVaultPda,
  placeOrderForRiskProfileInstruction,
  setFeeConfigInstruction,
  setGlobalPauseInstruction,
  setMarketPauseInstruction,
  transferMarketAdminInstruction,
  YDELTA_PROGRAM_ID,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import { USDC_MINT } from './_fixtures.ts';
import { expectCustomError, YdeltaError } from './_errors.ts';
import {
  setupGlobalConfig,
  setupMarket,
  setupRiskProfile,
  setupVault,
  unpauseMarket,
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
    await unpauseMarket(bk, admin, market.publicKey);
    await setupVault(bk, admin);
    curator = await bk.fundedKeypair();
    await setupRiskProfile(bk, admin, curator.publicKey, { maxLtvBps: 8_000 });
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
            ltvBufferBps: 100,
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

  it('CreateRiskProfile by non-vault-admin → VaultAdminRequired', async () => {
    await expectCustomError(
      bk.send(
        [
          createRiskProfileInstruction({
            payer: outsider.publicKey,
            mint: USDC_MINT,
            profileId: 99,
            curator: outsider.publicKey,
            maxLtvBps: 5_000,
            maxTermSeconds: 30 * 86_400,
          }),
        ],
        [outsider],
      ),
      YdeltaError.VaultAdminRequired,
      'outsider tries to allocate a risk profile',
    );
  });

  it('PlaceOrderForRiskProfile by non-curator → VaultCuratorRequired', async () => {
    // The split-payer ix needs BOTH signatures, but the curator signature is
    // the one checked against `profile.curator`. We sign with `outsider` in
    // both slots; the curator gate should reject.
    await expectCustomError(
      bk.send(
        [
          placeOrderForRiskProfileInstruction({
            feePayer: outsider.publicKey,
            curator: outsider.publicKey, // not the real curator
            mint: USDC_MINT,
            market: market.publicKey,
            profileId: 0,
            rateBps: 500,
            termSeconds: 30 * 86_400,
          }),
        ],
        [outsider],
      ),
      YdeltaError.VaultCuratorRequired,
      'outsider impersonates curator on PlaceOrder',
    );
  });

  it('CancelOrderForRiskProfile by non-curator → VaultCuratorRequired', async () => {
    await expectCustomError(
      bk.send(
        [
          cancelOrderForRiskProfileInstruction({
            feePayer: outsider.publicKey,
            curator: outsider.publicKey,
            mint: USDC_MINT,
            market: market.publicKey,
            profileId: 0,
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
