#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/fsend-site"
bun run build
bunx wrangler pages deploy --project-name fsend --branch main ./dist/
