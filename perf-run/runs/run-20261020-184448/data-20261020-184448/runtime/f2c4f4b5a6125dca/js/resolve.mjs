// Module hooks for the external Node runtime.
//
// `register.mjs` registers this file with node:module.
//
// 1) resolve: the bare specifier `host-api` always maps to this runtime's shim.
//    Everything else (relative plugin imports, node: builtins, packages) goes
//    through Node's normal resolution via nextResolve.
//
// 2) load: on Node versions where `.js` is unconditionally treated as CommonJS
//    (before syntax auto-detection became the default), an ESM plugin written as
//    `plugin.js` would fail with "Cannot use import statement outside a module".
//    The panel contract is Node >= 20.6 and repo plugins are `.js` ESM, so when
//    nextLoad says `commonjs` we look for top-level ESM syntax and, if present,
//    tell Node to parse the file as a module instead. CJS files are untouched;
//    `.mjs`/`"type":"module"` files are already `module` and also untouched.

import { readFile } from 'node:fs/promises';

import { looksLikeEsm } from './bus.mjs';

const HOST_API_URL = new URL('./host-api/index.mjs', import.meta.url).href;

export async function resolve(specifier, context, nextResolve) {
  if (specifier === 'host-api') {
    // shortCircuit: true means "this is the final URL, do not try to load it
    // as a package". Without it Node would still perform default resolution.
    return { url: HOST_API_URL, shortCircuit: true };
  }
  return nextResolve(specifier, context);
}

function isJsFileUrl(url) {
  try {
    const parsed = new URL(url);
    return parsed.protocol === 'file:' && parsed.pathname.endsWith('.js');
  } catch {
    return false;
  }
}

export async function load(url, context, nextLoad) {
  const result = await nextLoad(url, context);
  if (!result || result.format !== 'commonjs' || !isJsFileUrl(url)) return result;

  let source = result.source;
  if (source === undefined || source === null) {
    try {
      source = await readFile(new URL(url), 'utf8');
    } catch {
      // Let Node report the real read error through its normal path.
      return result;
    }
  }
  const text =
    typeof source === 'string' ? source : Buffer.from(source).toString('utf8');
  if (!looksLikeEsm(text)) return result;

  return { format: 'module', source: text, shortCircuit: true };
}
