import { Router, Route } from "@solidjs/router";
import { Layout } from "./components/Layout";
import { HomePage } from "./pages/HomePage";
import { SendPage } from "./pages/SendPage";
import { ReceivePage } from "./pages/ReceivePage";
import { AboutPage } from "./pages/AboutPage";

export default function App() {
  return (
    <Router>
      <Route
        path="/"
        component={() => (
          <Layout>
            <HomePage />
          </Layout>
        )}
      />
      <Route
        path="/send"
        component={() => (
          <Layout>
            <SendPage />
          </Layout>
        )}
      />
      <Route
        path="/receive"
        component={() => (
          <Layout>
            <ReceivePage />
          </Layout>
        )}
      />
      <Route
        path="/receive/:code"
        component={() => (
          <Layout>
            <ReceivePage />
          </Layout>
        )}
      />
      <Route
        path="/about"
        component={() => (
          <Layout>
            <AboutPage />
          </Layout>
        )}
      />
    </Router>
  );
}
