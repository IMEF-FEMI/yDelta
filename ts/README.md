# `@ydelta/sdk`

**TypeScript SDK for [yDelta](https://github.com/IMEF-FEMI/yDelta) — optimized fixed-rate, marginfi-backed lending on Solana.**

This package wraps the on-chain yDelta program: instruction builders, account
decoders over raw `getAccountInfo` bytes, and the math/oracle helpers needed to
build and read transactions. For how the protocol itself works — the two-sided
orderbook, sub-vaults, spread-over-bank rates, LTV decoupling — see the
[protocol README](https://github.com/IMEF-FEMI/yDelta#readme) and
[`docs/protocol-design.md`](https://github.com/IMEF-FEMI/yDelta/blob/main/docs/protocol-design.md).

## Install

```bash
yarn add @ydelta/sdk
# or
npm install @ydelta/sdk
```

Works with `@solana/web3.js ^1.95`.

## Quickstart

```ts
import { Connection, PublicKey } from '@solana/web3.js';
import {
  YDELTA_PROGRAM_ID,
  fetchMarket,
  fetchVault,
  placeOrderInstruction,
} from '@ydelta/sdk';

const conn = new Connection('https://api.mainnet-beta.solana.com', 'confirmed');
const market = await fetchMarket(conn, new PublicKey('9mSq5qvdKPJdNE8T8UMALrkuLxisw9eed87z33sWCUcv'));
// Vaults are keyed by the marginfi BANK (the market's debt-side lending pool),
// not by mint.
const vault = await fetchVault(conn, market.header.debtLendingPool);
```

## What the SDK ships

- **Instruction builders** for every yDelta instruction (`placeOrderInstruction`, `repayInstruction`, `convertP2poolToFixedInstruction`, …).
- **Account decoders** (`decodeMarket`, `decodeGlobalVault`, `decodeLoanFixed`, …) over the raw `getAccountInfo` bytes.
- **Helpers** for LTV math, marginfi share/atom conversion, and oracle price reads.
- **The IDL** (`idl/ydelta.json`) for off-chain tooling and codegen.

## Operator scripts

The repo also ships `tsx` operator scripts (bootstrap, market init, vault
deposit/withdraw, cranks, liquidation, etc.) runnable via the package scripts —
e.g. `yarn bootstrap`, `yarn match-crank`, `yarn liquidate`. See
[`ts/scripts/`](https://github.com/IMEF-FEMI/yDelta/tree/main/ts/scripts) and the
`scripts` block in [`package.json`](https://github.com/IMEF-FEMI/yDelta/blob/main/package.json).

## Source & program

- SDK source: [`ts/src/`](https://github.com/IMEF-FEMI/yDelta/tree/main/ts/src)
- On-chain program: [`programs/ydelta/`](https://github.com/IMEF-FEMI/yDelta/tree/main/programs/ydelta)

## License

See the [yDelta repository](https://github.com/IMEF-FEMI/yDelta).
