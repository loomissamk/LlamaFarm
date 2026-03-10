import { Navigate, Route, Routes } from 'react-router-dom';
import Layout from './components/layout/Layout';
import Dashboard from './pages/Dashboard';
import AgentChat from './pages/AgentChat';
import Tools from './pages/Tools';
import Cron from './pages/Cron';
import Integrations from './pages/Integrations';
import Memory from './pages/Memory';
import Config from './pages/Config';
import Models from './pages/Cost';
import Logs from './pages/Logs';
import Doctor from './pages/Doctor';

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route path="/" element={<Dashboard />} />
        <Route path="/agent" element={<AgentChat />} />
        <Route path="/tools" element={<Tools />} />
        <Route path="/cron" element={<Cron />} />
        <Route path="/integrations" element={<Integrations />} />
        <Route path="/memory" element={<Memory />} />
        <Route path="/config" element={<Config />} />
        <Route path="/models" element={<Models />} />
        <Route path="/cost" element={<Navigate to="/models" replace />} />
        <Route path="/logs" element={<Logs />} />
        <Route path="/doctor" element={<Doctor />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
