import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

// Relative to this file, not the cwd: the SSG pass runs Vite from elsewhere.
const FILE = fileURLToPath(new URL("../.buildinfo.json", import.meta.url));

function commitSha() {
    // Pages git builds and GitHub Actions hand us the SHA; a checkout deployed
    // by deploy.sh has to ask git, and a tarball with no .git has neither.
    const fromEnv = process.env.CF_PAGES_COMMIT_SHA || process.env.GITHUB_SHA;
    if (fromEnv) return fromEnv;

    try {
        return execFileSync("git", ["rev-parse", "HEAD"], {
            encoding: "utf8",
            stdio: ["ignore", "pipe", "ignore"],
        }).trim();
    } catch {
        return null;
    }
}

// Preformatted, so it reads the same whatever the locale or timezone.
function formatUtc(date) {
    return `${date.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

export function generateBuildInfo() {
    const sha = commitSha();
    const now = new Date();
    return {
        commit: sha ? sha.slice(0, 7) : null,
        commitFull: sha,
        builtAt: formatUtc(now),
        // Epoch millis too: the "... ago" line has to be recomputed in the browser,
        // so it needs the instant rather than the rendered string.
        builtAtMs: now.getTime(),
    };
}

/** The CLI version in fsend-cli/Cargo.toml, which the download page names. */
export function cliVersion() {
    const manifest = readFileSync(new URL("../../fsend-cli/Cargo.toml", import.meta.url), "utf8");
    return /^version = "(.+)"$/m.exec(manifest)[1];
}

export function loadBuildInfo() {
    try {
        return JSON.parse(readFileSync(FILE, "utf8"));
    } catch {
        return generateBuildInfo();
    }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
    const info = generateBuildInfo();
    writeFileSync(FILE, `${JSON.stringify(info, null, 2)}\n`);
    console.log(`build info: ${info.commit ?? "unknown"} @ ${info.builtAt}`);
}
