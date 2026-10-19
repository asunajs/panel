// config() is a normal host-api RPC; the fake panel answers it in the test.
import { config } from 'host-api';

const cfg = config();
console.log('config-json ' + JSON.stringify(cfg));

export function on_tick() {}
