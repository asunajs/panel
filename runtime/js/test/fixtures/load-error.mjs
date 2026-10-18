// Plugin whose top-level code throws: the runner must report ready.ok=false.

import { log } from 'host-api';
log('plugin load starts');
throw new Error('plugin-load-boom');
