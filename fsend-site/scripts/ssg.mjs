import { readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

const DIST = "dist";
const SSR_OUT = ".ssg";
const ROUTES = ["/", "/send", "/receive", "/download"];

const { renderRoute } = await import(
    pathToFileURL(join(process.cwd(), SSR_OUT, "entry-server.js")).href
);

// Each route supplies its own title/description/canonical, so drop the
// placeholders from the shell, or every page ships two of each.
const template = (await readFile(join(DIST, "index.html"), "utf8"))
    .replace(/\s*<title>[\s\S]*?<\/title>/, "")
    .replace(/\s*<meta[^>]*name="description"[^>]*>/, "")
    .replace(/\s*<link[^>]*rel="canonical"[^>]*>/, "");

/** The `name` attribute of the first tag in `html` that `tag` matches. */
const attr = (html, tag, name) =>
    new RegExp(`${name}="([^"]*)"`).exec(tag.exec(html)?.[0] ?? "")?.[1];

/** `template` with its share tags set to `fields`, by property or name. */
const share = (template, fields) =>
    Object.entries(fields).reduce(
        (out, [key, value]) =>
            value === undefined
                ? out
                : out.replace(
                      new RegExp(`(<meta[^>]*="${key}"[^>]*content=")[^"]*"`),
                      (_, start) => `${start}${value}"`,
                  ),
        template,
    );

for (const url of ROUTES) {
    const { html, head } = renderRoute(url);
    const title = /<title[^>]*>([\s\S]*?)<\/title>/.exec(head)?.[1];
    const description = attr(head, /<meta[^>]*name="description"[^>]*>/, "content");
    const canonical = attr(head, /<link[^>]*rel="canonical"[^>]*>/, "href");

    const out = share(template, {
        "og:title": title,
        "twitter:title": title,
        "og:description": description,
        "twitter:description": description,
        "og:url": canonical,
    })
        // Replacer functions, so a `$&` in the content is not a pattern.
        .replace("</head>", () => `  ${head}\n  </head>`)
        .replace('<div id="root"></div>', () => `<div id="root">${html}</div>`);

    // Flat files, not <route>/index.html: Pages serves `foo.html` at `/foo` but
    // 308s `/foo` to `/foo/` when the file is a directory index, which would
    // disagree with our canonicals and sitemap.
    const file = url === "/" ? join(DIST, "index.html") : join(DIST, `${url.slice(1)}.html`);
    await mkdir(dirname(file), { recursive: true });
    await writeFile(file, out);

    console.log(`  prerendered ${url.padEnd(9)} ${(out.length / 1024).toFixed(1)} kB`);
}

await rm(SSR_OUT, { recursive: true, force: true });
