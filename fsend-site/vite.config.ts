import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import devtools from "solid-devtools/vite";
import { cliVersion, loadBuildInfo } from "./scripts/buildinfo.mjs";

const buildInfo = loadBuildInfo();

export default defineConfig(({ isSsrBuild }) => ({
    define: {
        __BUILD_COMMIT__: JSON.stringify(buildInfo.commit),
        __BUILD_COMMIT_FULL__: JSON.stringify(buildInfo.commitFull),
        __BUILD_TIME__: JSON.stringify(buildInfo.builtAt),
        __BUILD_TIMESTAMP__: JSON.stringify(buildInfo.builtAtMs),
        __CLI_VERSION__: JSON.stringify(cliVersion()),
    },
    // The SSR build is the prerender pass.
    plugins: [...(isSsrBuild ? [] : [devtools()]), solidPlugin({ ssr: isSsrBuild }), tailwindcss()],
    server: {
        port: 3000,
    },
    optimizeDeps: {
        include: ["solid-devtools/setup"],
    },
    build: {
        target: "esnext",
    },
    worker: {
        format: "es",
    },
}));
