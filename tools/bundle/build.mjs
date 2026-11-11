// Generates vendor/llrt/bundle/js, which llrt_core/build.rs compiles into bytecode.
// Upstream build.mjs also bundles the AWS SDK and the test harness; the panel needs neither.
import * as esbuild from "esbuild";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "../..");
const LLRT = path.join(REPO, "vendor/llrt");
const SRC = path.join(LLRT, "llrt_core/src/modules/js");
const OUT = path.join(LLRT, "bundle/js");

const EXTERNAL = [
  "assert", "async_hooks", "buffer", "child_process", "console", "crypto",
  "dgram", "dns", "events", "fs", "module", "net", "os", "path", "perf_hooks",
  "process", "stream", "string_decoder", "timers", "tty", "url", "util", "zlib",
  "llrt:codec", "llrt:timezone", "llrt:qjs", "llrt:util", "llrt:xml",
].flatMap((m) => [m, `node:${m}`]);

await fs.rm(OUT, { recursive: true, force: true });
await fs.mkdir(OUT, { recursive: true });

const result = await esbuild.build({
  absWorkingDir: LLRT,
  entryPoints: {
    stream: path.join(SRC, "stream.ts"),
    "stream/promises": path.join(SRC, "stream/promises.ts"),
  },
  outdir: OUT,
  bundle: true,
  splitting: false,
  minify: true,
  keepNames: true,
  sourcemap: false,
  target: "es2023",
  platform: "browser",
  format: "esm",
  external: EXTERNAL,
  nodePaths: [path.join(HERE, "node_modules")],
  logLevel: "info",
  plugins: [
    {
      name: "require-process",
      setup(build) {
        build.onResolve({ filter: /^process\/$/ }, () => ({ path: "process", external: true }));
      },
    },
  ],
  metafile: true,
});

for (const [name, meta] of Object.entries(result.metafile.outputs)) {
  console.log(`${name}  ${meta.bytes} bytes`);
}