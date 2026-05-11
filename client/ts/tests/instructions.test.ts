import { expect } from 'chai';
import { Keypair, PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import {
  YDELTA_PROGRAM_ID,
  YdeltaInstructionTag,
} from '../src/utils';
import {
  createClaimSeatInstruction,
  createDepositInstruction,
  createPlaceOrderInstruction,
  createCheckMaturityLiquidatableInstruction,
} from '../src/ydelta/instructions';
import { Side, OrderType, OrderKind } from '../src/ydelta/types';

const MARKET = new PublicKey('11111111111111111111111111111112');
const MINT = new PublicKey('11111111111111111111111111111114');
const MARGINFI_GROUP = Keypair.generate().publicKey;
const MARGINFI_PROGRAM = Keypair.generate().publicKey;
const DEBT_BANK = Keypair.generate().publicKey;
const COLLATERAL_BANK = Keypair.generate().publicKey;
const TOKEN_PROGRAM = new PublicKey('TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');

describe('instruction builders', () => {
  it('ClaimSeat: data is exactly [discriminator]', () => {
    const payer = Keypair.generate().publicKey;
    const ix = createClaimSeatInstruction({ payer, market: MARKET });
    expect(ix.programId.equals(YDELTA_PROGRAM_ID)).to.equal(true);
    expect(ix.data.length).to.equal(1);
    expect(ix.data[0]).to.equal(YdeltaInstructionTag.ClaimSeat);
  });

  it('Deposit: discriminator prefix matches tag 2', () => {
    const payer = Keypair.generate().publicKey;
    const traderToken = Keypair.generate().publicKey;
    const ix = createDepositInstruction(
      {
        payer,
        market: MARKET,
        mint: MINT,
        debtMint: MINT,
        traderToken,
        tokenProgram: TOKEN_PROGRAM,
        marginfiGroup: MARGINFI_GROUP,
        bank: DEBT_BANK,
        liquidityVault: Keypair.generate().publicKey,
        marginfiProgram: MARGINFI_PROGRAM,
      },
      { amountAtoms: new BN(1_000_000), traderIndexHint: null },
    );
    expect(ix.data[0]).to.equal(YdeltaInstructionTag.Deposit);
    // amountAtoms (u64 LE) immediately after tag.
    const amount = ix.data.readBigUInt64LE(1);
    expect(amount).to.equal(1_000_000n);
    // traderIndexHint None → single 0 byte.
    expect(ix.data[9]).to.equal(0);
    expect(ix.data.length).to.equal(10);
  });

  it('PlaceOrder: fields serialize in the documented order', () => {
    const payer = Keypair.generate().publicKey;
    const ix = createPlaceOrderInstruction(
      {
        payer,
        market: MARKET,
        marginfiGroup: MARGINFI_GROUP,
        debtBank: DEBT_BANK,
        collateralBank: COLLATERAL_BANK,
        debtOracles: [Keypair.generate().publicKey],
        collateralOracles: [Keypair.generate().publicKey],
        debtLiquidityVault: Keypair.generate().publicKey,
        debtBankLiquidityVaultAuthority: Keypair.generate().publicKey,
        borrowerDebtToken: Keypair.generate().publicKey,
        debtMint: MINT,
        tokenProgram: TOKEN_PROGRAM,
        marginfiProgram: MARGINFI_PROGRAM,
      },
      {
        seatIndexHint: null,
        side: Side.Bid,
        orderType: OrderType.Limit,
        flags: 0,
        kind: OrderKind.Primary,
        rateBps: 500,
        termSeconds: 86_400,
        principalAtoms: new BN(1_000),
        collateralAtoms: new BN(2_000),
        askingPriceAtoms: new BN(0),
        lastValidUnixTs: new BN(0),
        borrowerLtvBps: null,
      },
    );
    expect(ix.data[0]).to.equal(YdeltaInstructionTag.PlaceOrder);
    // seat_index_hint None → 1 byte, then side at offset 2
    expect(ix.data[1]).to.equal(0); // None tag
    expect(ix.data[2]).to.equal(Side.Bid);
    expect(ix.data[3]).to.equal(OrderType.Limit);
    expect(ix.data[4]).to.equal(0); // flags
    expect(ix.data[5]).to.equal(OrderKind.Primary);
    // rate_bps u16 at offset 6
    expect(ix.data.readUInt16LE(6)).to.equal(500);
  });

  it('CheckMaturityLiquidatable: data is exactly [tag 41]', () => {
    const payer = Keypair.generate().publicKey;
    const ix = createCheckMaturityLiquidatableInstruction({
      payer,
      market: MARKET,
      sequence: 1n,
      debtBank: DEBT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
    });
    expect(ix.data.length).to.equal(1);
    expect(ix.data[0]).to.equal(YdeltaInstructionTag.CheckMaturityLiquidatable);
    expect(YdeltaInstructionTag.CheckMaturityLiquidatable).to.equal(41);
  });
});
