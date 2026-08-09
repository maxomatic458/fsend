cd fsend-site
bun run build && bunx wrangler pages deploy --project-name fsend --branch main ./dist/
