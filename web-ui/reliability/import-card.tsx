import React from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import ImportProgressCard from '../src/components/ImportProgressCard';
import '../src/index.css';

const repoId = new URLSearchParams(window.location.search).get('repo');
if (!repoId) throw new Error('Missing fixture repository ID');
createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={new QueryClient()}>
      <BrowserRouter>
        <ImportProgressCard repoId={repoId} repoName="history" />
      </BrowserRouter>
    </QueryClientProvider>
  </React.StrictMode>,
);
