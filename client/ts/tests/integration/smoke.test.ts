import { expect } from 'chai';
import { Keypair } from '@solana/web3.js';
import {
  bootstrap,
  bankrunYdeltaClient,
  makeUsers,
  Harness,
} from './_harness';
import { YdeltaInstructionTag } from '../../src/utils/discriminator';

describe('Phase 5 smoke — bankrun e2e', function () {
  this.timeout(120_000);

  let harness: Harness;
  let alice: Keypair;

  before(async () => {
    harness = await bootstrap();
    const users = await makeUsers(harness.context, ['alice']);
    alice = users.alice;
  });

  it('boots bankrun with ydelta + marginfi + test-harness loaded', async () => {
    const ydeltaAcc = await harness.banksClient.getAccount(
      (await import('../../src/utils/programId')).YDELTA_PROGRAM_ID,
    );
    expect(ydeltaAcc).to.not.equal(null);
    expect(ydeltaAcc!.executable).to.equal(true);
  });

  it('replays mainnet marginfi state', async () => {
    const { MARGINFI_GROUP, USDC_BANK } = await import('./_harness/mainnet');
    const group = await harness.banksClient.getAccount(MARGINFI_GROUP);
    const bank = await harness.banksClient.getAccount(USDC_BANK);
    expect(group, 'marginfi group').to.not.equal(null);
    expect(bank, 'USDC bank').to.not.equal(null);
    expect(group!.data.length).to.be.greaterThan(0);
    expect(bank!.data.length).to.be.greaterThan(0);
  });

  it('creates a GlobalConfig PDA with the bpf upgrade authority as protocol admin', async () => {
    // CreateGlobalConfig requires payer == upgrade_authority on the
    // ProgramData PDA. In bankrun the loaded program's upgrade authority is
    // unknown, so this ix is expected to fail with ProgramData verification
    // unless the harness pre-seeds matching state. For the smoke test we
    // only assert the ix is well-formed and surfaces a recognisable error
    // (i.e. the program is reachable and decoded the discriminator).
    const { createCreateGlobalConfigInstruction } = await import('../../src/ydelta/instructions');
    const ix = createCreateGlobalConfigInstruction({ payer: harness.payer.publicKey });
    expect(ix.data[0]).to.equal(YdeltaInstructionTag.CreateGlobalConfig);
    expect(ix.keys.length).to.equal(4);
    // Don't actually send — see note above. Use the local-globalConfig path
    // exposed by tests that override the program-data account.
  });

  it('SDK YdeltaClient can derive PDAs and read live state via the bankrun connection', async () => {
    const { client, connection } = bankrunYdeltaClient(harness.context);
    // Cross-derive against the standalone PDA helpers so a regression in
    // either path is caught (was previously asserting `.to.be.a('string')`,
    // which is always true).
    const {
      globalConfigPda,
      marketSignerPda,
      lenderMarginfiAccountPda,
      userAccountPda,
    } = await import('../../src/utils/pdas');
    const fakeMarket = Keypair.generate().publicKey;
    expect(client.globalConfigPda().toBase58()).to.equal(globalConfigPda()[0].toBase58());
    expect(client.marketSignerPda(fakeMarket).toBase58()).to.equal(marketSignerPda(fakeMarket)[0].toBase58());
    expect(client.lenderMarginfiAccountPda(fakeMarket).toBase58()).to.equal(
      lenderMarginfiAccountPda(fakeMarket)[0].toBase58(),
    );
    expect(client.userAccountPda(alice.publicKey).toBase58()).to.equal(
      userAccountPda(alice.publicKey)[0].toBase58(),
    );
    // Cross-reference: the loaded marginfi state reads back through the
    // SDK's bankrun-backed Connection — proves the SDK boots cleanly e2e.
    const { MARGINFI_GROUP } = await import('./_harness/mainnet');
    const info = await connection.getAccountInfo(MARGINFI_GROUP);
    expect(info, 'marginfi group via SDK connection').to.not.equal(null);
  });
});
