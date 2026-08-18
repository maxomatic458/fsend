import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import devtools from "solid-devtools/vite";
import { loadBuildInfo } from "./scripts/buildinfo.mjs";

const buildInfo = loadBuildInfo();

export default defineConfig(({ isSsrBuild }) => ({
  define: {
    __BUILD_COMMIT__: JSON.stringify(buildInfo.commit),
    __BUILD_COMMIT_FULL__: JSON.stringify(buildInfo.commitFull),
    __BUILD_TIME__: JSON.stringify(buildInfo.builtAt),
    __BUILD_TIMESTAMP__: JSON.stringify(buildInfo.builtAtMs),
  },
  // The prerender pass needs Solid compiled for the server (generate: "ssr"),
  // so the same config serves both builds.
  plugins: [
    ...(isSsrBuild ? [] : [devtools()]),
    solidPlugin({ ssr: isSsrBuild }),
    tailwindcss(),
  ],
  server: {
    port: 3000,
  },
  build: {
    target: "esnext",
  },
}));
