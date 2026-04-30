import { Router, Route } from '@solidjs/router';
import { Layout } from './components/Layout';
import { HomePage } from './pages/HomePage';
import { SendPage } from './pages/SendPage';
import { ReceivePage } from './pages/ReceivePage';

export default function App() {
  return (
    <Router>
      <Route path="/" component={() => <Layout><HomePage /></Layout>} />
      <Route path="/send" component={() => <Layout><SendPage /></Layout>} />
      <Route path="/receive" component={() => <Layout><ReceivePage /></Layout>} />
      <Route path="/receive/:code" component={() => <Layout><ReceivePage /></Layout>} />
    </Router>
  );
}
