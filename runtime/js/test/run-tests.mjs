#!/usr/bin/env node
// CI entrypoint: `node runtime/js/test/run-tests.mjs`.
//
// Thin wrapper around Node's built-in test runner. Keeping this file means CI
// does not need to know which test files currently exist. Bun is exercised from
// resolve.test.mjs only when a `bun` binary is on PATH (skipped otherwise).

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const files = fs
  .readdirSync(here)
  .filter((name) => name.endsWith('.test.mjs'))
  .sort()
  .map((name) => path.join(here, name));

if (files.length === 0) {
  console.error('run-tests: no *.test.mjs files found');
  process.exit(1);
}

const child = spawn(process.execPath, ['--test', ...files], {
  cwd: path.resolve(here, '..'),
  stdio: 'inherit',
  env: process.env,
});

child.on('error', (e) => {
  console.error(`run-tests: failed to start node --test: ${e.message}`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (signal) {
    console.error(`run-tests: node --test terminated by ${signal}`);
    process.exit(1);
  }
  process.exit(code === null ? 1 : code);
});
