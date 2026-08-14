import type { JSX } from "solid-js";
import { Router, Route } from "@solidjs/router";
import { Layout } from "./components/Layout";
import { HomePage } from "./pages/HomePage";
import { SendPage } from "./pages/SendPage";
import { ReceivePage } from "./pages/ReceivePage";

const page = (Component: () => JSX.Element) => () => (
  <Layout>
    <Component />
  </Layout>
);

export default function App() {
  return (
    <Router>
      <Route path="/" component={page(HomePage)} />
      <Route path="/send" component={page(SendPage)} />
      <Route path="/receive" component={page(ReceivePage)} />
      <Route path="/receive/:code" component={page(ReceivePage)} />
    </Router>
  );
}
