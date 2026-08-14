import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import devtools from "solid-devtools/vite";

export default defineConfig(({ isSsrBuild }) => ({
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
