// AIVX 前端入口（React 18 + TS）。P8d：AIGX 式侧边栏布局 + 六页。

import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter, Route, Routes } from 'react-router-dom';
import { DevicesPage } from './pages/Devices';
import { LivePage } from './pages/Live';
import { RulesPage } from './pages/Dashboard';
import { AlarmsPage } from './pages/Alarms';
import { RecordingsPage } from './pages/Recordings';
import { AgentPage } from './pages/Agent';
import { Sidebar } from './components/Sidebar';
import { AuthGate } from './components/AuthGate';
import { RealApiClient } from './api';
import { initTheme } from './lib/theme';
import './App.css';
import './aivx.css';

// 初始化主题（system/light/dark 三态，system 跟随系统明暗；防 FOUC）
initTheme();

const api = new RealApiClient();

function App() {
  return (
    <AuthGate api={api}>
      {(authEnabled: boolean, onLogout: () => void) => (
        <div className="app-container">
          <Sidebar authEnabled={authEnabled} onLogout={onLogout} />
          <main className="main-content">
            <div className="page-fade-enter">
              <Routes>
                <Route path="/" element={<LivePage api={api} />} />
                <Route path="/live" element={<LivePage api={api} />} />
                <Route path="/devices" element={<DevicesPage api={api} />} />
                <Route path="/rules" element={<RulesPage api={api} />} />
                <Route path="/alarms" element={<AlarmsPage api={api} />} />
                <Route path="/recordings" element={<RecordingsPage api={api} />} />
                <Route path="/agent" element={<AgentPage />} />
              </Routes>
            </div>
          </main>
        </div>
      )}
    </AuthGate>
  );
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <BrowserRouter>
      <App />
    </BrowserRouter>
  </React.StrictMode>,
);
