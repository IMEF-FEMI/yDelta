import { expect } from 'chai';
import * as fs from 'fs';
import * as path from 'path';
import { YDELTA_IDL } from '../../idl/idl';
import { POD_BYTE_SIZES, structByteSize, assertPodSizes } from '../../idl/size';
import { YdeltaInstructionTag } from '../src/utils/discriminator';
import { YDELTA_PROGRAM_ID } from '../src/utils/programId';

describe('yDelta IDL', () => {
  it('contains exactly 42 instructions covering tags 0..=41', () => {
    expect(YDELTA_IDL.instructions.length).to.equal(42);
    const seen = new Set<number>();
    for (const ix of YDELTA_IDL.instructions) {
      expect(ix.discriminant.type).to.equal('u8');
      expect(seen.has(ix.discriminant.value), `dup tag ${ix.discriminant.value}`).to.equal(false);
      seen.add(ix.discriminant.value);
    }
    for (let i = 0; i <= 41; i++) {
      expect(seen.has(i), `missing tag ${i}`).to.equal(true);
    }
  });

  it('instruction names match `YdeltaInstructionTag` 1:1', () => {
    for (const ix of YDELTA_IDL.instructions) {
      const fromEnum = YdeltaInstructionTag[ix.discriminant.value];
      expect(fromEnum, `tag ${ix.discriminant.value}`).to.equal(ix.name);
    }
  });

  it('declares all five top-level account types', () => {
    const names = YDELTA_IDL.accounts.map((a) => a.name).sort();
    expect(names).to.deep.equal(
      ['GlobalConfig', 'GlobalVaultFixed', 'LoanFixed', 'MarketFixed', 'UserAccountFixed'].sort(),
    );
    for (const a of YDELTA_IDL.accounts) {
      expect(a.discriminator.length, `${a.name} disc length`).to.equal(8);
    }
  });

  it('error codes cover YdeltaError (0..58) and AdapterError (100..106)', () => {
    const codes = new Set(YDELTA_IDL.errors.map((e) => e.code));
    for (let i = 0; i <= 58; i++) expect(codes.has(i), `ydelta error ${i}`).to.equal(true);
    for (let i = 100; i <= 106; i++) expect(codes.has(i), `adapter error ${i}`).to.equal(true);
  });

  it('every `{defined: X}` reference resolves to a type or account', () => {
    const known = new Set<string>([
      ...YDELTA_IDL.types.map((t) => t.name),
      ...YDELTA_IDL.accounts.map((a) => a.name),
    ]);
    const refs = new Set<string>();
    const walk = (t: unknown): void => {
      if (!t || typeof t !== 'object') return;
      if ('defined' in (t as Record<string, unknown>)) {
        refs.add(String((t as { defined: unknown }).defined));
      }
      if ('option' in (t as Record<string, unknown>)) walk((t as { option: unknown }).option);
      if ('vec' in (t as Record<string, unknown>)) walk((t as { vec: unknown }).vec);
      if ('array' in (t as Record<string, unknown>)) {
        const arr = (t as { array: unknown }).array;
        if (Array.isArray(arr)) walk(arr[0]);
      }
    };
    for (const ix of YDELTA_IDL.instructions) for (const a of ix.args) walk(a.type);
    for (const acc of YDELTA_IDL.accounts) for (const f of acc.type.fields) walk(f.type);
    for (const td of YDELTA_IDL.types) {
      if (td.type.kind === 'struct') for (const f of td.type.fields) walk(f.type);
    }
    for (const r of refs) expect(known.has(r), `unresolved \`{defined: '${r}'}\``).to.equal(true);
  });

  it('metadata pins the on-chain program id', () => {
    expect(YDELTA_IDL.metadata.address).to.equal(YDELTA_PROGRAM_ID.toBase58());
  });

  it('every Pod struct/account field-sum equals the canonical byte size', () => {
    // assertPodSizes throws with a precise diff if any struct mismatches.
    expect(() => assertPodSizes(YDELTA_IDL)).to.not.throw();
    // Belt-and-braces: also check individually so failures point at the right struct.
    for (const { name, bytes } of POD_BYTE_SIZES) {
      expect(structByteSize(YDELTA_IDL, name), `${name} size`).to.equal(bytes);
    }
  });

  it('generated ydelta.json matches the in-memory IDL', () => {
    const json = JSON.parse(
      fs.readFileSync(path.join(__dirname, '..', '..', 'idl', 'ydelta.json'), 'utf8'),
    );
    expect(json.name).to.equal(YDELTA_IDL.name);
    expect(json.instructions.length).to.equal(YDELTA_IDL.instructions.length);
    expect(json.accounts.length).to.equal(YDELTA_IDL.accounts.length);
    expect(json.errors.length).to.equal(YDELTA_IDL.errors.length);
    expect(json.metadata.address).to.equal(YDELTA_IDL.metadata.address);
  });
});
