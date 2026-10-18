// Classic (QuickJS-style) plugin: no top-level import/export.
// The runner must capture this top-level function declaration as on_tick.

const PLUGIN = 'classic';
let ticks = 0;

host.log('classic loaded', PLUGIN);

function on_tick(payload) {
  ticks += 1;
  host.log('classic tick', ticks, payload.seq, payload.ts);
  console.log('classic console', ticks);
  return { ok: true };
}
