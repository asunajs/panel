// Script mode must reject scheduling functions with a clear error.

import { register_cron } from 'host-api';

register_cron('nope', '@every 1s', () => {});
