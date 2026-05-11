import { expect } from 'chai';
import {
  AddressLookupTableProgram,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionMessage,
  VersionedTransaction,
} from '@solana/web3.js';
import {
  bootstrap,
  bankrunYdeltaClient,
  Harness,
} from './_harness';
import {
  chunkKeys,
  dedupKeys,
  uniqueKeysFromIxs,
} from '../../src/helpers/lut';

/** Bankrun's BanksClient signs/sends txs without a connection.sendTransaction
 *  proxy, so we build + send the LUT create/extend ixs by hand here rather
 *  than going through `createAndExtendLut` (which targets a real Connection). */
describe('e2e ALT — create + extend + read', function () {
  this.timeout(60_000);
  let harness: Harness;

  before(async () => {
    harness = await bootstrap();
  });

  it('createLookupTable + extendLookupTable lands and is readable', async () => {
    const banks = harness.context.banksClient;
    const slot = Number(await banks.getSlot());
    const [createIx, lookupTable] = AddressLookupTableProgram.createLookupTable({
      authority: harness.payer.publicKey,
      payer: harness.payer.publicKey,
      recentSlot: slot - 1,
    });

    const someKeys = Array.from({ length: 8 }, () => Keypair.generate().publicKey);
    const extendIx = AddressLookupTableProgram.extendLookupTable({
      lookupTable,
      authority: harness.payer.publicKey,
      payer: harness.payer.publicKey,
      addresses: someKeys,
    });

    const blockhash = (await banks.getLatestBlockhash())![0];
    const tx = new Transaction().add(createIx, extendIx);
    tx.recentBlockhash = blockhash;
    tx.feePayer = harness.payer.publicKey;
    tx.sign(harness.payer);
    const result = await banks.tryProcessTransaction(tx);
    expect(result.result, JSON.stringify(result.meta?.logMessages ?? [])).to.equal(null);

    // Read the LUT account back from bankrun.
    const acc = await banks.getAccount(lookupTable);
    expect(acc, 'lookup table account').to.not.equal(null);
    expect(acc!.data.length).to.be.greaterThan(56);
  });

  it('uniqueKeysFromIxs / dedupKeys / chunkKeys are end-to-end consistent', async () => {
    const ixs = [
      SystemProgram.transfer({
        fromPubkey: harness.payer.publicKey,
        toPubkey: Keypair.generate().publicKey,
        lamports: 1,
      }),
      SystemProgram.transfer({
        fromPubkey: harness.payer.publicKey,
        toPubkey: Keypair.generate().publicKey,
        lamports: 1,
      }),
    ];
    const unique = uniqueKeysFromIxs(ixs);
    // payer + 2 recipients + system_program = 4 distinct keys
    expect(unique.length).to.equal(4);
    expect(dedupKeys([...unique, ...unique])).to.have.length(4);
    expect(chunkKeys(unique, 2)).to.have.length(2);
  });

  it('SDK Connection proxy through BankrunProvider supports v0 message compilation', async () => {
    const { connection } = bankrunYdeltaClient(harness.context);
    const blockhash = (await harness.context.banksClient.getLatestBlockhash())![0];
    // Use a rent-exempt-minimum transfer so bankrun's runtime accepts it.
    const rent = await harness.context.banksClient.getRent();
    const minBal = Number(rent.minimumBalance(0n));
    const ix = SystemProgram.transfer({
      fromPubkey: harness.payer.publicKey,
      toPubkey: Keypair.generate().publicKey,
      lamports: minBal,
    });
    const message = new TransactionMessage({
      payerKey: harness.payer.publicKey,
      recentBlockhash: blockhash,
      instructions: [ix],
    }).compileToV0Message();
    const tx = new VersionedTransaction(message);
    tx.sign([harness.payer]);
    // SDK connection is a polyfill — verify it exposes getAccountInfo on the
    // payer (sanity check that bankrun is wired before sending).
    const info = await connection.getAccountInfo(harness.payer.publicKey);
    expect(info, 'payer info via SDK connection').to.not.equal(null);
    // Send via bankrun directly (the polyfill doesn't implement sendTransaction).
    const result = await harness.context.banksClient.tryProcessTransaction(tx);
    expect(result.result, JSON.stringify(result.meta?.logMessages ?? [])).to.equal(null);
    void PublicKey;
  });
});
