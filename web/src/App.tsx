import { Navigate, Route, Routes } from 'react-router-dom';
import Layout from './components/layout/Layout';
import Dashboard from './pages/Dashboard';
import FederationPage from './pages/Federation';
import Tools from './pages/Tools';
import Cron from './pages/Cron';
import Integrations from './pages/Integrations';
import Memory from './pages/Memory';
import Config from './pages/Config';
import Logs from './pages/Logs';
import Runs from './pages/Runs';
import Doctor from './pages/Doctor';
import WorkspaceIde from './pages/WorkspaceIde';
import WorkspaceFiles from './pages/WorkspaceFiles';
import WorkspacePrompts from './pages/WorkspacePrompts';
import DatabasePage from './pages/Database';

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route path="/" element={<Dashboard />} />
        <Route path="/agent" element={null} />
        <Route path="/federation" element={<FederationPage />} />
        <Route path="/tools" element={<Tools />} />
        <Route path="/cron" element={<Cron />} />
        <Route path="/integrations" element={<Integrations />} />
        <Route path="/memory" element={<Memory />} />
        <Route path="/workspace" element={<WorkspaceIde />} />
        <Route path="/workspace/files" element={<WorkspaceFiles />} />
        <Route path="/workspace/prompts" element={<WorkspacePrompts />} />
        <Route path="/database" element={<DatabasePage />} />
        <Route path="/config" element={<Config />} />
        <Route path="/models" element={<Navigate to="/integrations" replace />} />
        <Route path="/cost" element={<Navigate to="/integrations" replace />} />
        <Route path="/runs" element={<Runs />} />
        <Route path="/logs" element={<Logs />} />
        <Route path="/doctor" element={<Doctor />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
