/**
 * Bundles src/lib to plain ESM with esbuild, then runs the suite under Node's
 * built-in test runner. Bundling (rather than mocking modules) means the tests
 * exercise the same code the browser ships.
 */
import { build } from "esbuild";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));

await build({
  entryPoints: [join(here, "entry.ts")],
  outfile: join(here, ".build/app.mjs"),
  bundle: true,
  format: "esm",
  platform: "browser",
  target: "es2022",
  // The app reads import.meta.env for the relay URL; the harness swaps the
  // WebSocket out from under it, so the value itself is irrelevant.
  define: { "import.meta.env": JSON.stringify({}) },
  logLevel: "warning",
});

// Explicit glob: the default discovery patterns would also pick up the
// harness modules under test/, which are not test files.
const child = spawn(
  process.execPath,
  ["--test", ...process.argv.slice(2), join(here, "**/*.test.mjs")],
  { stdio: "inherit" },
);
child.on("exit", (code) => process.exit(code ?? 1));
