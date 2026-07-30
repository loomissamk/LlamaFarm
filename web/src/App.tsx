import { lazy } from 'react';
import { Navigate, Route, Routes } from 'react-router-dom';
import Layout from './components/layout/Layout';

const Dashboard = lazy(() => import('./pages/Dashboard'));
const FederationPage = lazy(() => import('./pages/Federation'));
const Tools = lazy(() => import('./pages/Tools'));
const Cron = lazy(() => import('./pages/Cron'));
const Integrations = lazy(() => import('./pages/Integrations'));
const Memory = lazy(() => import('./pages/Memory'));
const Config = lazy(() => import('./pages/Config'));
const Logs = lazy(() => import('./pages/Logs'));
const Runs = lazy(() => import('./pages/Runs'));
const Doctor = lazy(() => import('./pages/Doctor'));
const WorkspaceIde = lazy(() => import('./pages/WorkspaceIde'));
const WorkspaceFiles = lazy(() => import('./pages/WorkspaceFiles'));
const WorkspacePrompts = lazy(() => import('./pages/WorkspacePrompts'));
const DatabasePage = lazy(() => import('./pages/Database'));
const NotFound = lazy(() => import('./pages/NotFound'));

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
        <Route path="*" element={<NotFound />} />
      </Route>
    </Routes>
  );
}
