import type { JSX } from "solid-js";
import { Layout } from "./components/Layout";
import { HomePage } from "./pages/HomePage";
import { SendPage } from "./pages/SendPage";
import { ReceivePage } from "./pages/ReceivePage";
import { DownloadPage } from "./pages/DownloadPage";

const page = (Component: () => JSX.Element) => () => (
  <Layout>
    <Component />
  </Layout>
);

/// Shared by the client router and the build-time prerender.
export const routes = [
  { path: "/", component: page(HomePage) },
  { path: "/send", component: page(SendPage) },
  { path: "/download", component: page(DownloadPage) },
  { path: "/receive", component: page(ReceivePage) },
  { path: "/receive/:code", component: page(ReceivePage) },
];
