#!/usr/bin/env bash
set -euo pipefail

# Resolve relative to this script, so it works from any directory.
cd "$(dirname "$0")/fsend-site"

bun run build
bunx wrangler pages deploy --project-name fsend --branch main ./dist/
