import { Keypair, PublicKey, Transaction } from '@solana/web3.js';
import { expect } from 'chai';
import {
  bootstrap,
  createGlobalConfigIfMissing,
  createUsdcWsolMarket,
  Harness,
  processTx,
  seedYdeltaProgramData,
  expectYdeltaError,
} from './_harness';
import {
  createClaimSeatInstruction,
  createCreateGlobalConfigInstruction,
} from '../../src/ydelta/instructions';
import { decodeError, findYdeltaErrorCodeInLogs } from '../../src/helpers/errors';

describe('e2e errors — Custom(u32) → YdeltaError mapping', function () {
  this.timeout(120_000);

  let harness: Harness;
  let market: PublicKey;

  before(async () => {
    harness = await bootstrap();
    seedYdeltaProgramData(harness.context, harness.payer);
    await createGlobalConfigIfMissing({ context: harness.context, payer: harness.payer });
    const r = await createUsdcWsolMarket({ context: harness.context, payer: harness.payer });
    market = r.market.publicKey;
  });

  it('CreateGlobalConfig fails with InvalidArgument when re-called', async () => {
    const ix = createCreateGlobalConfigInstruction({ payer: harness.payer.publicKey });
    const res = await processTx(harness.context.banksClient, new Transaction().add(ix), harness.payer);
    expectYdeltaError(res, 'InvalidArgument');
  });

  it('ClaimSeat against a non-existent market fails with IncorrectAccount or AccountNotInitialized', async () => {
    // Pick a random pubkey that has no account allocated yet — the loader's
    // owner check rejects it with `IncorrectAccount`, or the runtime rejects
    // it earlier with `AccountNotInitialized` if no AccountInfo exists.
    const fakeMarket = Keypair.generate().publicKey;
    const ix = createClaimSeatInstruction({ payer: harness.payer.publicKey, market: fakeMarket });
    const res = await processTx(harness.context.banksClient, new Transaction().add(ix), harness.payer);
    expect(res.result, 'tx must fail').to.not.equal(null);
    const logs = (res.meta?.logMessages ?? []) as string[];
    const code = findYdeltaErrorCodeInLogs(logs);
    if (code !== null) {
      // Hit the ydelta loader — assert a recognised ydelta error.
      const e = decodeError({ code });
      expect(e, 'decoded ydelta error').to.not.equal(null);
      expect(e!.name).to.be.oneOf(['IncorrectAccount', 'InvalidArgument']);
    } else {
      // Did not reach ydelta — verify the runtime reason matches the expected
      // pre-program failure shape (uninitialised account / owner mismatch).
      const resultStr = JSON.stringify(res.result);
      const acceptable = [
        'AccountNotInitialized',
        'IncorrectProgramId',
        'InvalidAccountOwner',
        'InvalidAccountData',
      ];
      const matched = acceptable.some((needle) => resultStr.includes(needle));
      expect(matched, `expected pre-program failure in ${resultStr}`).to.equal(true);
    }
  });

  it('decodeError maps AlreadyClaimedSeat (5) for a duplicate claim', async () => {
    // First claim — may succeed (fresh market) or already be claimed if a
    // sibling test ran first. Either outcome unblocks us; we only need
    // ONE failing claim to assert the decoder.
    const res1 = await processTx(
      harness.context.banksClient,
      new Transaction().add(createClaimSeatInstruction({ payer: harness.payer.publicKey, market })),
      harness.payer,
    );
    // The second claim is guaranteed to fail with AlreadyClaimedSeat.
    const res2 = await processTx(
      harness.context.banksClient,
      new Transaction().add(createClaimSeatInstruction({ payer: harness.payer.publicKey, market })),
      harness.payer,
    );
    const failing = res1.result === null ? res2 : res1;
    expect(failing.result, 'one claim must have failed').to.not.equal(null);
    const logs = (failing.meta?.logMessages ?? []) as string[];
    const decoded = decodeError({ logs });
    if (!decoded) {
      // eslint-disable-next-line no-console
      console.error('logs for failing claim:', logs);
    }
    expect(decoded, 'decoded ydelta error').to.not.equal(null);
    expect(decoded!.name).to.equal('AlreadyClaimedSeat');
    expect(decoded!.code).to.equal(5);
  });
});
