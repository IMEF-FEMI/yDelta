export const SECONDS_PER_MINUTE = 60;
export const SECONDS_PER_HOUR = 3600;
export const SECONDS_PER_DAY = 86_400;
export const SECONDS_PER_YEAR = 31_536_000;

export function nowUnixSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

export function maturesAt(startUnix: number, termSeconds: number): number {
  return startUnix + termSeconds;
}

export function isMatured(maturesAtUnix: number, nowUnix: number): boolean {
  return nowUnix > maturesAtUnix;
}

export function pastGrace(
  maturesAtUnix: number,
  gracePeriodSeconds: number,
  nowUnix: number,
): boolean {
  return nowUnix > maturesAtUnix + gracePeriodSeconds;
}
