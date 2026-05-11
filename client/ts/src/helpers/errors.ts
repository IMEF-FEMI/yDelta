import { SendTransactionError, SimulatedTransactionResponse } from '@solana/web3.js';
import { errorFromCode } from '../ydelta/errors';
import { YDELTA_PROGRAM_ID } from '../utils/programId';
import { PublicKey } from '@solana/web3.js';

export type DecodedYdeltaError = {
  code: number;
  name: string;
  message: string;
};

const HEX_RE = /custom program error:\s*0x([0-9a-fA-F]+)/;
const DEC_RE = /custom program error:\s*(\d+)/;
const PROGRAM_FAIL_RE = /Program ([A-Za-z0-9]+) failed:\s*custom program error:\s*0x([0-9a-fA-F]+)/;

/** Parse a `Custom(u32)` program error from a single log line. Returns the
 *  numeric code or `null` if the line doesn't carry one. */
export function parseCustomCodeFromLog(line: string): number | null {
  const m = line.match(HEX_RE);
  if (m) return parseInt(m[1], 16);
  const d = line.match(DEC_RE);
  if (d) return parseInt(d[1], 10);
  return null;
}

/** Walk a log array and return the first ydelta-program `Custom(u32)` it finds,
 *  matched against `programId` (the canonical `YDELTA_PROGRAM_ID` unless
 *  overridden). Returns `null` if no matching error appears. */
export function findYdeltaErrorCodeInLogs(
  logs: readonly string[],
  programId: PublicKey = YDELTA_PROGRAM_ID,
): number | null {
  const target = programId.toBase58();
  let pendingFromOurProgram = false;
  for (const line of logs) {
    // "Program <id> invoke [..]" marks entry into a program.
    const invokeMatch = line.match(/Program ([A-Za-z0-9]+) invoke \[\d+]/);
    if (invokeMatch) {
      pendingFromOurProgram = invokeMatch[1] === target;
      continue;
    }
    // "Program <id> failed: custom program error: 0x..." is the explicit form.
    const explicit = line.match(PROGRAM_FAIL_RE);
    if (explicit && explicit[1] === target) {
      return parseInt(explicit[2], 16);
    }
    // Bare "Program log:" or similar that mentions a code while we're inside
    // our program's frame.
    if (pendingFromOurProgram) {
      const code = parseCustomCodeFromLog(line);
      if (code !== null) return code;
    }
  }
  return null;
}

/** Resolve a Custom code to a `YdeltaError` record. */
export function decodeYdeltaCode(code: number): DecodedYdeltaError | null {
  const e = errorFromCode(code);
  if (!e) return null;
  return { code, name: e.name, message: e.message };
}

export type DecodeErrorInput =
  | { logs: readonly string[]; programId?: PublicKey }
  | { error: unknown; programId?: PublicKey }
  | { simulation: SimulatedTransactionResponse; programId?: PublicKey }
  | { code: number }
  | { hex: string };

/** Best-effort decoder: pass logs, a caught `SendTransactionError`, a
 *  `simulateTransaction` response, a raw `Custom(u32)` code, or a hex string
 *  like `"0x2f"`. Returns `null` when nothing matches yDelta. */
export function decodeError(input: DecodeErrorInput): DecodedYdeltaError | null {
  if ('code' in input) {
    return decodeYdeltaCode(input.code);
  }
  if ('hex' in input) {
    const v = input.hex.replace(/^0x/i, '');
    const code = parseInt(v, 16);
    return Number.isFinite(code) ? decodeYdeltaCode(code) : null;
  }
  if ('simulation' in input) {
    const logs = input.simulation.logs ?? [];
    const code = findYdeltaErrorCodeInLogs(logs, input.programId);
    return code !== null ? decodeYdeltaCode(code) : null;
  }
  if ('logs' in input) {
    const code = findYdeltaErrorCodeInLogs(input.logs, input.programId);
    return code !== null ? decodeYdeltaCode(code) : null;
  }
  // `error` path: pull logs off a web3.js SendTransactionError, otherwise
  // search the stringified message for a hex code.
  const err = input.error;
  if (err instanceof SendTransactionError) {
    // Newer web3.js versions expose `getLogs()`; older expose `.logs`.
    const sendErr = err as SendTransactionError & { logs?: string[] };
    const logs = sendErr.logs ?? [];
    const code = findYdeltaErrorCodeInLogs(logs, input.programId);
    if (code !== null) return decodeYdeltaCode(code);
  }
  const target = (input.programId ?? YDELTA_PROGRAM_ID).toBase58();
  const text = typeof err === 'string' ? err : (err as { message?: string } | null)?.message ?? '';
  if (!text.includes(target)) return null;
  const code = parseCustomCodeFromLog(text);
  return code !== null ? decodeYdeltaCode(code) : null;
}
