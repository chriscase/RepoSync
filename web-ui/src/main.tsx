import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import SessionBoundary from './SessionBoundary';
import App from './App';
import '@appforgeapps/uiforge/styles.css';
import './uiforge-overrides.css';
import './index.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <SessionBoundary>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </SessionBoundary>
  </React.StrictMode>
);
