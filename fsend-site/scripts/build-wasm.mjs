import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SITE = fileURLToPath(new URL("..", import.meta.url));
const OUT = join(SITE, "src/lib/proto/pkg");
const TARGET = "wasm32-unknown-unknown";
const PROFILE = "wasm";

const run = (cmd, args, options = {}) =>
  execFileSync(cmd, args, { cwd: SITE, stdio: "inherit", ...options });

export function buildWasm() {
  run("cargo", [
    "build",
    "--package",
    "fsend-proto-wasm",
    "--target",
    TARGET,
    "--profile",
    PROFILE,
  ]);
  const { target_directory } = JSON.parse(
    run("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
      stdio: ["ignore", "pipe", "inherit"],
      encoding: "utf8",
    }),
  );
  run("wasm-bindgen", [
    "--target",
    "web",
    "--out-dir",
    OUT,
    join(target_directory, TARGET, PROFILE, "fsend_proto_wasm.wasm"),
  ]);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) buildWasm();
