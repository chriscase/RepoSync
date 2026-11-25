import { useEffect, useState, useSyncExternalStore, type ReactNode } from 'react';
import { api } from '../api';
import { getStoredUser } from '../utils/auth';

function subscribeToStorage(update: () => void) {
  window.addEventListener('storage', update);
  return () => window.removeEventListener('storage', update);
}

function candidateSession() {
  const user = getStoredUser();
  return user && user.role !== 'admin' ? null : localStorage.getItem('session_token');
}

function AccessDenied() {
  return (
    <div role="alert" className="text-center py-12">
      <h2 className="text-xl font-semibold text-gray-100 mb-2">Access Denied</h2>
      <p className="text-gray-400">You need admin privileges to view this page.</p>
    </div>
  );
}

function VerifyAdministrator({ children }: { children: ReactNode }) {
  const [allowed, setAllowed] = useState<boolean | null>(null);
  useEffect(() => {
    let current = true;
    api.getMe().then(
      user => { if (current) setAllowed(user.role === 'admin'); },
      () => { if (current) setAllowed(false); },
    );
    return () => { current = false; };
  }, []);

  if (allowed === null) {
    return <div role="status" className="text-center py-12 text-gray-400">Verifying administrator access…</div>;
  }
  return allowed ? <>{children}</> : <AccessDenied />;
}

export default function AdministratorRoute({ children }: { children: ReactNode }) {
  const token = useSyncExternalStore(subscribeToStorage, candidateSession, candidateSession);
  // Each new session verifies its own identity before administrative effects are mounted.
  return token ? <VerifyAdministrator key={token}>{children}</VerifyAdministrator> : <AccessDenied />;
}
