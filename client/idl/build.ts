#!/usr/bin/env -S npx ts-node
/* eslint-disable no-console */
import * as fs from 'fs';
import * as path from 'path';
import { YDELTA_IDL } from './idl';
import { instructions } from './instructions';
import { accounts } from './accounts';
import { types } from './shared-types';
import { errors } from './errors';
import { assertPodSizes, POD_BYTE_SIZES, structByteSize } from './size';

const OUT_PATH = path.join(__dirname, 'ydelta.json');

function validate(): void {
  // Instruction tags must be contiguous 0..=N-1; derive N from the list so
  // adding a new tag doesn't require touching this validator.
  const seen = new Set<number>();
  for (const ix of instructions) {
    if (seen.has(ix.discriminant.value)) {
      throw new Error(`duplicate instruction tag ${ix.discriminant.value} (${ix.name})`);
    }
    if (ix.discriminant.value < 0 || ix.discriminant.value > 255) {
      throw new Error(`tag ${ix.discriminant.value} out of u8 range (${ix.name})`);
    }
    seen.add(ix.discriminant.value);
  }
  for (let tag = 0; tag < instructions.length; tag++) {
    if (!seen.has(tag)) {
      throw new Error(`missing instruction tag ${tag}`);
    }
  }

  // Account discriminators must be 8 bytes.
  for (const acc of accounts) {
    if (acc.discriminator.length !== 8) {
      throw new Error(`account ${acc.name} discriminator is ${acc.discriminator.length} bytes (expected 8)`);
    }
    for (const b of acc.discriminator) {
      if (b < 0 || b > 255) {
        throw new Error(`account ${acc.name} discriminator byte ${b} out of u8 range`);
      }
    }
  }

  // Error codes unique.
  const errCodes = new Set<number>();
  for (const e of errors) {
    if (errCodes.has(e.code)) {
      throw new Error(`duplicate error code ${e.code} (${e.name})`);
    }
    errCodes.add(e.code);
  }

  // Every `{defined: 'X'}` reference must resolve to a type or account.
  const known = new Set<string>([...types.map((t) => t.name), ...accounts.map((a) => a.name)]);
  const seenDefined = new Set<string>();
  function collect(t: unknown): void {
    if (!t || typeof t !== 'object') return;
    if ('defined' in (t as Record<string, unknown>)) {
      seenDefined.add(String((t as { defined: unknown }).defined));
    }
    if ('option' in (t as Record<string, unknown>)) collect((t as { option: unknown }).option);
    if ('vec' in (t as Record<string, unknown>)) collect((t as { vec: unknown }).vec);
    if ('array' in (t as Record<string, unknown>)) {
      const arr = (t as { array: unknown }).array;
      if (Array.isArray(arr)) collect(arr[0]);
    }
  }
  for (const ix of instructions) {
    for (const a of ix.args) collect(a.type);
  }
  for (const acc of accounts) for (const f of acc.type.fields) collect(f.type);
  for (const td of types) {
    if (td.type.kind === 'struct') for (const f of td.type.fields) collect(f.type);
  }
  for (const ref of seenDefined) {
    if (!known.has(ref)) {
      throw new Error(`unresolved type reference: '{defined: "${ref}"}'`);
    }
  }
}

function summarize(): void {
  console.log(`yDelta IDL:`);
  console.log(`  instructions: ${instructions.length} (tags 0..=${instructions.length - 1})`);
  console.log(`  accounts:     ${accounts.length}`);
  console.log(`  types:        ${types.length}`);
  console.log(`  errors:       ${errors.length}`);
  console.log(`  Pod byte sizes (verified):`);
  for (const { name, bytes } of POD_BYTE_SIZES) {
    const actual = structByteSize(YDELTA_IDL, name);
    console.log(`    ${name.padEnd(28)} ${String(bytes).padStart(4)}B  (idl: ${actual}B)`);
  }
}

function main(): void {
  validate();
  // Walk every Pod struct/account and verify the field list sums to the
  // canonical byte size — catches missing/stale padding.
  assertPodSizes(YDELTA_IDL);
  summarize();
  fs.writeFileSync(OUT_PATH, JSON.stringify(YDELTA_IDL, null, 2) + '\n');
  console.log(`wrote ${path.relative(process.cwd(), OUT_PATH)} (${fs.statSync(OUT_PATH).size} bytes)`);
}

main();
