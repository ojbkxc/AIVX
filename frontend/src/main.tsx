// AIVX 前端入口（React 18 + TS）。

import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter, Route, Routes } from 'react-router-dom';
import { DevicesPage } from './pages/Devices';
import { RealApiClient } from './api';

const api = new RealApiClient();

function App() {
  return (
    <div className="app">
      <nav className="nav">
        <span className="nav-brand">AIVX</span>
        <a href="/devices">设备</a>
      </nav>
      <main>
        <Routes>
          <Route path="/devices" element={<DevicesPage api={api} />} />
          <Route path="/" element={<DevicesPage api={api} />} />
        </Routes>
      </main>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <BrowserRouter>
      <App />
    </BrowserRouter>
  </React.StrictMode>,
);