// Classic one-shot script (no import/export). Uses global host + patched console.

host.log('classic script start');
console.log('classic script console');
globalThis.__classic_script_done = true;
