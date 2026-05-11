import { Idl } from './format';
import { instructions } from './instructions';
import { accounts } from './accounts';
import { types } from './shared-types';
import { errors } from './errors';

/** Canonical yDelta IDL. `build.ts` serialises it to `ydelta.json`. */
export const YDELTA_IDL: Idl = {
  version: '0.1.0',
  name: 'ydelta',
  instructions,
  accounts,
  types,
  errors,
  metadata: {
    address: '9Tcnk3xQKXeoSdY7ovTyGtGGFbBxraQR7TDhRE2UyXRT',
    origin: 'hand-written',
  },
};
