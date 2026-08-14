/**
 * Bakes each route into a static HTML file.
 *
 * `vite build` emits the client bundle plus an empty index.html shell, then
 * `vite build --ssr` emits a Node build of the same app. This script renders
 * every route with that build and writes the result back into dist/, so
 * crawlers get real markup instead of <div id="root"></div>.
 */
import { readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

const DIST = "dist";
const SSR_OUT = ".ssg";
const ROUTES = ["/", "/send", "/receive"];

const { renderRoute } = await import(
  pathToFileURL(join(process.cwd(), SSR_OUT, "entry-server.js")).href
);

// Each route supplies its own title/description/canonical, so drop the
// placeholders from the shell — otherwise every page ships two of each.
const template = (await readFile(join(DIST, "index.html"), "utf8"))
  .replace(/\s*<title>[\s\S]*?<\/title>/, "")
  .replace(/\s*<meta[^>]*name="description"[^>]*>/, "")
  .replace(/\s*<link[^>]*rel="canonical"[^>]*>/, "");

for (const url of ROUTES) {
  const { html, head } = renderRoute(url);

  const out = template
    .replace("</head>", `  ${head}\n  </head>`)
    .replace('<div id="root"></div>', `<div id="root">${html}</div>`);

  // Flat files, not <route>/index.html: Pages serves `foo.html` at `/foo` but
  // 308s `/foo` to `/foo/` when the file is a directory index, which would
  // disagree with our canonicals and sitemap.
  const file =
    url === "/" ? join(DIST, "index.html") : join(DIST, `${url.slice(1)}.html`);
  await mkdir(dirname(file), { recursive: true });
  await writeFile(file, out);

  console.log(
    `  prerendered ${url.padEnd(9)} ${(out.length / 1024).toFixed(1)} kB`,
  );
}

await rm(SSR_OUT, { recursive: true, force: true });
