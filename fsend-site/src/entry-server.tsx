import { renderToString, getAssets } from "solid-js/web";
import { MetaProvider } from "@solidjs/meta";
import { StaticRouter } from "@solidjs/router";
import { routes } from "./routes";

/**
 * Renders one route to static HTML at build time. `head` holds the tags the
 * page declared through @solidjs/meta, already deduplicated.
 */
export function renderRoute(url: string) {
  const html = renderToString(() => (
    <MetaProvider>
      <StaticRouter url={url}>{routes}</StaticRouter>
    </MetaProvider>
  ));

  return { html, head: getAssets() };
}
