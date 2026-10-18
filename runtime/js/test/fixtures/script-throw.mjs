// Top-level failure in script mode: ready has already been emitted, the error
// must be reported in `done` and the process must exit 1.

import { log } from 'host-api';

log('before throw');
throw new Error('script-top-level-boom');
