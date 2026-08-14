/**
 * Share links look like /receive/<code>, which has no file behind it.
 *
 * Cloudflare Pages can't express this in _redirects — it has no support for
 * 200 rewrites — and adding a root 404.html switches off the automatic SPA
 * fallback that used to cover it. Functions are matched ahead of the static
 * 404 handling, so this serves the prerendered receive page instead, leaving
 * every other unmatched path free to return a real 404.
 *
 * The client router reads the code back out of location.pathname.
 */
export async function onRequest({ request, env }) {
  const url = new URL(request.url);
  url.pathname = "/receive";
  return env.ASSETS.fetch(new Request(url, request));
}
