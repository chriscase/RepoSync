import { useQuery } from '@tanstack/react-query';
import { clientSession } from '../session';
import { getStoredUser } from '../utils/auth';
import ImportProgressMeter from '../components/ImportProgressMeter';
import { useId } from 'react';
import { useState, useRef, useEffect, useCallback, type ReactNode } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';
import { api, type AuthorMapping, type ImportStatus, type WizardSetupConfig } from '../api';
import ImportPhaseGraphic from '../components/ImportPhaseGraphic';
import ServerMonitor from '../components/ServerMonitor';
import { importPhaseLabel, importStateLabel } from '../importProgress';
import { canCancelSetupImport, canStartSetupImport, useSetupImport } from '../useSetupImport';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface WizardData {
  // SVN
  svn_url: string;
  svn_username: string;
  svn_password: string;
  svn_password_env: string;
  svn_layout: 'standard' | 'custom';
  svn_trunk_path: string;
  svn_branches_path: string;
  svn_tags_path: string;

  // Git
  git_provider: 'github' | 'gitea';
  git_api_url: string;
  git_repo: string;
  git_token: string;
  git_token_env: string;
  git_default_branch: string;

  // Sync
  sync_mode: 'direct' | 'pr';
  sync_auto_merge: boolean;
  sync_tags: boolean;
  pr_title_prefix: string;
  pr_labels: string;
  pr_reviewers: string;
  pr_auto_merge: boolean;

  // File Policy / LFS
  max_file_size_mb: number;
  lfs_threshold_mb: number;
  lfs_patterns: string;
  ignore_patterns: string;

  // Identity
  identity_email_domain: string;
  identity_mapping_file: string;
  identity_mappings: AuthorMapping[];

  // Web & Auth
  web_listen: string;
  web_auth_mode: 'simple' | 'github_oauth' | 'both';
  web_admin_password_env: string;
  web_oauth_client_id: string;
  web_oauth_client_secret_env: string;
  web_oauth_allowed_users: string;

  // Daemon
  daemon_poll_interval: number;
  daemon_log_level: string;
  daemon_data_dir: string;

  // Notifications
  notif_slack_webhook_env: string;
  notif_email_smtp: string;
  notif_email_from: string;
  notif_email_recipients: string;
}

const DEFAULT_DATA: WizardData = {
  svn_url: '',
  svn_username: '',
  svn_password: '',
  svn_password_env: '',
  svn_layout: 'standard',
  svn_trunk_path: 'trunk',
  svn_branches_path: 'branches',
  svn_tags_path: 'tags',

  git_provider: 'github',
  git_api_url: 'https://api.github.com',
  git_repo: '',
  git_token: '',
  git_token_env: '',
  git_default_branch: 'main',

  sync_mode: 'direct',
  sync_auto_merge: true,
  sync_tags: true,
  pr_title_prefix: '[svn-sync]',
  pr_labels: '',
  pr_reviewers: '',
  pr_auto_merge: false,

  max_file_size_mb: 0,
  lfs_threshold_mb: 0,
  lfs_patterns: '',
  ignore_patterns: '',

  identity_email_domain: '',
  identity_mapping_file: '',
  identity_mappings: [],

  web_listen: '0.0.0.0:8080',
  web_auth_mode: 'simple',
  web_admin_password_env: 'REPOSYNC_ADMIN_PASSWORD',
  web_oauth_client_id: '',
  web_oauth_client_secret_env: '',
  web_oauth_allowed_users: '',

  daemon_poll_interval: 60,
  daemon_log_level: 'info',
  daemon_data_dir: '/var/lib/reposync',

  notif_slack_webhook_env: '',
  notif_email_smtp: '',
  notif_email_from: '',
  notif_email_recipients: '',
};

const STEPS = [
  { label: 'Welcome', short: 'Start' },
  { label: 'SVN Repository', short: 'SVN' },
  { label: 'Git Provider', short: 'Git' },
  { label: 'Sync Settings', short: 'Sync' },
  { label: 'Identity Mapping', short: 'Identity' },
  { label: 'Server & Auth', short: 'Server' },
  { label: 'Review', short: 'Review' },
  { label: 'Import', short: 'Import' },
];

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

type Errors = Record<string, string>;

// Each check runs on the step that shows its field, so its error appears next to that field
// (#264, G12.9). Step 3 is Sync Settings, step 4 Identity Mapping, step 5 Server & Auth.
function validateStep(step: number, data: WizardData, opts: { adminPasswordSet?: boolean } = {}): Errors {
  const errors: Errors = {};

  if (step === 1) {
    if (!data.svn_url.trim()) errors.svn_url = 'SVN URL is required';
    else if (!/^(svn|https?):\/\/.+/.test(data.svn_url.trim()))
      errors.svn_url = 'Must start with svn://, http://, or https://';
    if (!data.svn_username.trim()) errors.svn_username = 'Username is required';
    if (!data.svn_trunk_path.trim()) errors.svn_trunk_path = 'Trunk path is required';
  }

  if (step === 2) {
    if (!data.git_api_url.trim()) errors.git_api_url = 'API URL is required';
    else if (!/^https?:\/\/.+/.test(data.git_api_url.trim()))
      errors.git_api_url = 'Must be a valid HTTP(S) URL';
    if (!data.git_repo.trim()) errors.git_repo = 'Repository is required';
    else if (!/^[^/]+\/[^/]+$/.test(data.git_repo.trim()))
      errors.git_repo = 'Must be in owner/repo format';
  }

  if (step === 3) {
    if (data.lfs_threshold_mb !== undefined && data.lfs_threshold_mb < 0)
      errors.lfs_threshold_mb = 'LFS threshold must be non-negative';
  }

  if (step === 4) {
    if (data.identity_email_domain && !/^[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/.test(data.identity_email_domain.trim()))
      errors.identity_email_domain = 'Must be a valid domain (e.g. company.com)';
  }

  if (step === 5) {
    if (!data.web_listen.trim()) errors.web_listen = 'Listen address is required';
    else if (!/^.+:\d+$/.test(data.web_listen.trim()))
      errors.web_listen = 'Must be in host:port format (e.g. 0.0.0.0:8080)';
    if (data.daemon_poll_interval < 1) errors.daemon_poll_interval = 'Must be at least 1 second';
    if (!data.daemon_data_dir.trim()) errors.daemon_data_dir = 'Data directory is required';
    // An admin password already stored needs no env var name.
    if ((data.web_auth_mode === 'simple' || data.web_auth_mode === 'both') && !opts.adminPasswordSet && !data.web_admin_password_env.trim())
      errors.web_admin_password_env = 'Admin password env var is required for simple auth';
  }

  return errors;
}

// ---------------------------------------------------------------------------
// Saved configuration read (#264, G12.10)
// ---------------------------------------------------------------------------

/** The read of the saved configuration: still reading, read, or failed with the client's error text. */
export type SavedConfigRead =
  | { status: 'reading' }
  | { status: 'loaded' }
  | { status: 'failed'; error: string; retrying: boolean };

/** After a failed read the form is turned off; Save is offered only after a successful read. */
function savedConfigView(status: SavedConfigRead['status']): { formOff: boolean; canSave: boolean } {
  return { formOff: status === 'failed', canSave: status === 'loaded' };
}

/** Wizard fields taken from the saved configuration (unchanged mapping, shared by the first read and Retry). */
function savedConfigFields(cfg: WizardSetupConfig): {
  fields: Partial<WizardData>;
  svnPasswordSet: boolean;
  gitTokenSet: boolean;
  adminPasswordSet: boolean;
} {
  const merged: Partial<WizardData> = {};

  if (cfg.svn_url) merged.svn_url = cfg.svn_url;
  if (cfg.svn_username) merged.svn_username = cfg.svn_username;
  if (cfg.svn_layout) merged.svn_layout = cfg.svn_layout as WizardData['svn_layout'];
  if (cfg.svn_trunk_path) merged.svn_trunk_path = cfg.svn_trunk_path;

  if (cfg.git_provider) merged.git_provider = cfg.git_provider as WizardData['git_provider'];
  if (cfg.git_api_url) merged.git_api_url = cfg.git_api_url;
  if (cfg.git_repo) merged.git_repo = cfg.git_repo;
  if (cfg.git_branch) merged.git_default_branch = cfg.git_branch;

  if (cfg.sync_mode) merged.sync_mode = cfg.sync_mode as WizardData['sync_mode'];
  if (cfg.auto_merge !== undefined) merged.sync_auto_merge = cfg.auto_merge;
  if (cfg.sync_tags !== undefined) merged.sync_tags = cfg.sync_tags;
  if (cfg.lfs_threshold > 0) merged.lfs_threshold_mb = cfg.lfs_threshold / (1024 * 1024);

  if (cfg.email_domain) merged.identity_email_domain = cfg.email_domain;

  if (cfg.listen) merged.web_listen = cfg.listen;
  if (cfg.auth_mode) merged.web_auth_mode = cfg.auth_mode as WizardData['web_auth_mode'];
  if (cfg.poll_interval > 0) merged.daemon_poll_interval = cfg.poll_interval;
  if (cfg.log_level) merged.daemon_log_level = cfg.log_level;
  if (cfg.data_dir) merged.daemon_data_dir = cfg.data_dir;

  // Settings only the configuration file sets. Pre-filling them means an
  // unchanged form does not report them as "not applied" on save.
  if (cfg.svn_branches_path !== undefined) merged.svn_branches_path = cfg.svn_branches_path;
  if (cfg.svn_tags_path !== undefined) merged.svn_tags_path = cfg.svn_tags_path;
  if (cfg.svn_password_env !== undefined) merged.svn_password_env = cfg.svn_password_env;
  if (cfg.git_token_env !== undefined) merged.git_token_env = cfg.git_token_env;
  if (cfg.admin_password_env !== undefined) merged.web_admin_password_env = cfg.admin_password_env;
  if (cfg.max_file_size !== undefined) merged.max_file_size_mb = cfg.max_file_size / (1024 * 1024);
  if (cfg.lfs_patterns) merged.lfs_patterns = cfg.lfs_patterns.join(', ');
  if (cfg.ignore_patterns) merged.ignore_patterns = cfg.ignore_patterns.join(', ');

  return {
    fields: merged,
    svnPasswordSet: !!cfg.svn_password_set,
    gitTokenSet: !!cfg.git_token_set,
    adminPasswordSet: !!cfg.admin_password_set,
  };
}

// eslint-disable-next-line react-refresh/only-export-components
export { DEFAULT_DATA, validateStep, savedConfigView, savedConfigFields, testConnectionClass, type WizardData };

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

export default function SetupWizard() {
  // The authenticated route owns missing-session navigation. Do not mount any
  // administrative queries, effects or forms without the current admin role.
  const session = clientSession().snapshot();
  const currentUser = getStoredUser();
  // Single-password login intentionally omits user; /auth/me resolves its
  // existing legacy-admin identity. Never infer admin from an absent role.
  const identity = useQuery({
    queryKey: ['admin-page-identity', session.epoch],
    queryFn: api.getMe,
    // Verify once per session epoch without discarding in-progress admin edits.
    refetchInterval: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    enabled: !!session.token && !currentUser,
    retry: false,
  });
  if (!currentUser && session.token && (identity.isPending || identity.isFetching)) {
    return <div role="status" className="text-center py-12 text-gray-400">Verifying administrator access…</div>;
  }
  const verifiedIdentity = session.token && identity.isSuccess && !identity.isFetching ? identity.data : null;
  if ((currentUser ?? verifiedIdentity)?.role !== 'admin') {
    return (
      <div className="text-center py-12" role="alert">
        <h2 className="rs-heading text-xl font-semibold text-gray-100 mb-2">Access Denied</h2>
        <p className="text-gray-400">You need admin privileges to view this page.</p>
      </div>
    );
  }
  return <AdminSetupWizard />;
}

function AdminSetupWizard() {
  const navigate = useNavigate();
  const location = useLocation();
  const resumeImport = location.state?.setupImportOpen === true;
  const [checkingOnEntry, setCheckingOnEntry] = useState(!resumeImport);
  const [step, setStep] = useState(() => resumeImport ? 7 : 0);
  // Reload this history entry on the same step even if the worker stopped while away.
  useEffect(() => {
    const open = step === 7;
    if (open !== resumeImport) {
      navigate({ pathname: location.pathname, search: location.search, hash: location.hash }, {
        replace: true, state: { ...location.state, setupImportOpen: open },
      });
    }
  }, [step, resumeImport, navigate, location]);
  const [data, setData] = useState<WizardData>({ ...DEFAULT_DATA });
  const [errors, setErrors] = useState<Errors>({});
  const [configSaved, setConfigSaved] = useState(false);
  const [saveError, setSaveError] = useState('');
  const [saveWarnings, setSaveWarnings] = useState<string[]>([]);
  const [configRead, setConfigRead] = useState<SavedConfigRead>({ status: 'reading' });
  const stepRef = useRef<HTMLDivElement>(null);

  // Track whether secrets are already saved on the server
  const [svnPasswordSaved, setSvnPasswordSaved] = useState(false);
  const [gitTokenSaved, setGitTokenSaved] = useState(false);
  const [adminPasswordSaved, setAdminPasswordSaved] = useState(false);
  // Track whether user has typed new values for password fields
  const [svnPasswordDirty, setSvnPasswordDirty] = useState(false);
  const [gitTokenDirty, setGitTokenDirty] = useState(false);

  const update = useCallback(
    (fields: Partial<WizardData>) => setData(prev => ({ ...prev, ...fields })),
    [],
  );

  const onEntryStatus = useCallback((status: ImportStatus) => {
    setCheckingOnEntry(false);
    const needsAttention = status.busy ||
      ['connecting', 'importing', 'verifying', 'final_push'].includes(status.phase) ||
      ['queued', 'running', 'cancel_requested', 'cancelling', 'reconciliation_required'].includes(status.lifecycle ?? '');
    if (needsAttention) setStep(current => current === 0 ? 7 : current);
  }, []);

  // Pre-populate wizard from saved config. A failed read is shown as failed and turns the
  // form off until a read succeeds (#264, G12.10); Retry reads again.
  const readSavedConfig = useCallback(async () => {
    try {
      const cfg: WizardSetupConfig = await api.getSetupConfig();
      const saved = savedConfigFields(cfg);
      if (saved.svnPasswordSet) setSvnPasswordSaved(true);
      if (saved.gitTokenSet) setGitTokenSaved(true);
      if (saved.adminPasswordSet) setAdminPasswordSaved(true);
      setData(prev => ({ ...prev, ...saved.fields }));
      setConfigRead({ status: 'loaded' });
    } catch (e: unknown) {
      setConfigRead({ status: 'failed', error: e instanceof Error ? e.message : 'Unknown error', retrying: false });
    }
  }, []);

  useEffect(() => {
    void readSavedConfig();
  }, [readSavedConfig]);

  const retrySavedConfig = () => {
    setConfigRead(prev => (prev.status === 'failed' ? { ...prev, retrying: true } : prev));
    void readSavedConfig();
  };

  const { formOff, canSave } = savedConfigView(configRead.status);

  useEffect(() => {
    stepRef.current?.querySelector<HTMLInputElement>('input, select, textarea')?.focus();
  }, [step]);

  const goNext = () => {
    if (formOff) return;
    const errs = validateStep(step, data, { adminPasswordSet: adminPasswordSaved });
    setErrors(errs);
    if (Object.keys(errs).length > 0) return;
    setCheckingOnEntry(false);
    setStep(s => Math.min(s + 1, STEPS.length - 1));
  };

  const goBack = () => {
    setErrors({});
    setStep(s => Math.max(s - 1, 0));
  };

  const handleSaveConfig = async () => {
    if (!canSave) return;
    setSaveError('');
    setSaveWarnings([]);
    try {
      const payload = {
        svn_url: data.svn_url,
        svn_username: data.svn_username,
        svn_password: data.svn_password || undefined,
        // Settings only the configuration file sets are always sent as shown,
        // cleared ones included, so the server can say which were not applied.
        svn_password_env: data.svn_password_env,
        svn_layout: data.svn_layout,
        svn_trunk_path: data.svn_trunk_path,
        svn_branches_path: data.svn_branches_path,
        svn_tags_path: data.svn_tags_path,
        git_provider: data.git_provider,
        git_api_url: data.git_api_url,
        git_repo: data.git_repo,
        git_token: data.git_token || undefined,
        git_token_env: data.git_token_env,
        git_default_branch: data.git_default_branch,
        sync_mode: data.sync_mode,
        sync_auto_merge: data.sync_auto_merge,
        sync_tags: data.sync_tags,
        max_file_size: Math.max(0, Math.round(data.max_file_size_mb * 1024 * 1024)),
        lfs_threshold: data.lfs_threshold_mb > 0 ? data.lfs_threshold_mb * 1024 * 1024 : undefined,
        lfs_patterns: data.lfs_patterns.split(',').map(s => s.trim()).filter(Boolean),
        ignore_patterns: data.ignore_patterns.split(',').map(s => s.trim()).filter(Boolean),
        identity_email_domain: data.identity_email_domain,
        identity_mapping_file: data.identity_mapping_file || undefined,
        identity_mappings: data.identity_mappings.length > 0 ? data.identity_mappings : undefined,
        daemon_poll_interval: data.daemon_poll_interval,
        daemon_log_level: data.daemon_log_level,
        daemon_data_dir: data.daemon_data_dir,
        web_listen: data.web_listen,
        web_admin_password_env: data.web_admin_password_env,
      };
      const result = await api.applyConfig(payload);
      if (result.ok) {
        setConfigSaved(true);
        setSaveWarnings(result.warnings || []);
      } else {
        setSaveError(result.message);
      }
    } catch (e: unknown) {
      setSaveError(e instanceof Error ? e.message : 'Unknown error');
    }
  };

  const renderStep = () => {
    switch (step) {
      case 0: return <WelcomeStep />;
      case 1: return <SvnStep data={data} update={update} errors={errors} svnPasswordSaved={svnPasswordSaved} svnPasswordDirty={svnPasswordDirty} onSvnPasswordChange={() => setSvnPasswordDirty(true)} />;
      case 2: return <GitStep data={data} update={update} errors={errors} gitTokenSaved={gitTokenSaved} gitTokenDirty={gitTokenDirty} onGitTokenChange={() => setGitTokenDirty(true)} />;
      case 3: return <SyncStep data={data} update={update} errors={errors} />;
      case 4: return <IdentityStep data={data} update={update} errors={errors} />;
      case 5: return <ServerAuthStep data={data} update={update} errors={errors} />;
      case 6: return (
        <ReviewStep
          data={data}
          configSaved={configSaved}
          saveError={saveError}
          saveWarnings={saveWarnings}
          onSave={handleSaveConfig}
          readStatus={configRead.status}
        />
      );
      case 7: return <ImportStep />;
      default: return null;
    }
  };

  return (
    <div className="min-h-screen bg-gray-900 py-8 px-4">
      <div className="max-w-7xl mx-auto">
        {/* Header */}
        <div className="text-center mb-8">
          <h1 className="rs-title text-3xl font-bold text-gray-100 tracking-wider font-display">RepoSync Setup</h1>
          <p className="text-gray-400 mt-2">Configure your SVN-to-Git synchronization in a few steps</p>
        </div>

        {/* Step Indicator */}
        <StepIndicator currentStep={step} steps={STEPS} onStepClick={(i) => { if (i < step) setStep(i); }} />

        {/* Step Content */}
        <div ref={stepRef} className="rs-sec mt-8 bg-gray-800 rounded-xl shadow-lg border border-gray-700 p-8">
          {step === 0 && checkingOnEntry && <WelcomeImportCheck onStatus={onEntryStatus} />}
          {step === STEPS.length - 1 ? (
            renderStep()
          ) : (
            <SavedConfigGate read={configRead} onRetry={retrySavedConfig}>
              {renderStep()}
            </SavedConfigGate>
          )}
        </div>

        {/* Navigation */}
        <div className="mt-6 flex items-center justify-between">
          <div className="flex items-center space-x-3">
            {step > 0 && step < STEPS.length - 1 && (
              <button onClick={goBack} className="rs-btn rs-btn--quiet px-5 py-2.5 border border-gray-600 text-gray-300 hover:bg-gray-700 rounded-lg text-sm font-medium transition-colors">
                Back
              </button>
            )}
          </div>
          <div className="flex items-center space-x-3">
            <button onClick={() => navigate(step === 0 ? '/login' : '/')} className="px-5 py-2.5 text-gray-400 hover:text-gray-200 text-sm font-medium transition-colors">
              {step === 0 ? 'Skip to Login' : 'Exit to Dashboard'}
            </button>
            {step < STEPS.length - 2 && (
              <button onClick={goNext} disabled={formOff} className="rs-btn rs-btn--primary px-6 py-2.5 bg-blue-600 rs-text-on-accent hover:bg-blue-700 rounded-lg text-sm font-medium transition-colors shadow-sm disabled:opacity-50">
                {step === 0 ? 'Get Started' : 'Next'}
              </button>
            )}
            {step === STEPS.length - 2 && configSaved && (
              <button onClick={goNext} className="rs-btn rs-btn--primary px-6 py-2.5 bg-blue-600 rs-text-on-accent hover:bg-blue-700 rounded-lg text-sm font-medium transition-colors shadow-sm">
                Next: Import History
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step Indicator
// ---------------------------------------------------------------------------

function StepIndicator({
  currentStep,
  steps,
  onStepClick,
}: {
  currentStep: number;
  steps: { label: string; short: string }[];
  onStepClick: (i: number) => void;
}) {
  return (
    <>
      {/* Desktop */}
      <div className="hidden md:flex items-center justify-between">
        {steps.map((s, i) => (
          <div key={i} className="flex items-center flex-1 last:flex-none">
            <button
              aria-current={i === currentStep ? 'step' : undefined}
              onClick={() => onStepClick(i)}
              disabled={i > currentStep}
              className="flex flex-col items-center group"
            >
              <div
                className={`w-9 h-9 rounded-full flex items-center justify-center text-sm font-semibold border-2 transition-all duration-200 ${
                  i < currentStep
                    ? 'bg-blue-600 border-blue-600 rs-text-on-accent'
                    : i === currentStep
                      ? 'border-blue-500 text-blue-400 bg-blue-500/10'
                      : 'border-gray-600 text-gray-300 bg-gray-800'
                }`}
              >
                {i < currentStep ? (
                  <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d="M5 13l4 4L19 7" />
                  </svg>
                ) : (
                  i + 1
                )}
              </div>
              <span
                className={`mt-1.5 text-xs font-medium transition-colors ${
                  i <= currentStep ? 'text-gray-300' : 'text-gray-300'
                }`}
              >
                {s.short}
              </span>
            </button>
            {i < steps.length - 1 && (
              <div
                className={`flex-1 h-0.5 mx-2 mt-[-1rem] transition-colors duration-200 ${
                  i < currentStep ? 'bg-blue-600' : 'bg-gray-700'
                }`}
              />
            )}
          </div>
        ))}
      </div>
      {/* Mobile */}
      <div className="md:hidden text-center">
        <span className="text-sm text-gray-400">
          Step {currentStep + 1} of {steps.length}:
        </span>
        <span className="ml-2 text-sm font-medium text-gray-200">{steps[currentStep].label}</span>
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------
// Shared Form Components
// ---------------------------------------------------------------------------

function FormField({
  label,
  name,
  value,
  onChange,
  error,
  placeholder,
  help,
  type = 'text',
  mono,
  required,
  disabled,
}: {
  label: string;
  name: string;
  value: string | number;
  onChange: (val: string) => void;
  error?: string;
  placeholder?: string;
  help?: string;
  type?: string;
  mono?: boolean;
  required?: boolean;
  disabled?: boolean;
}) {
  return (
    <div>
      <label htmlFor={name} className="block text-sm font-medium text-gray-300 mb-1">
        {label}
        {required && <span aria-hidden="true" className="text-red-400 ml-1">*</span>}
      </label>
      <input
        id={name}
        name={name}
        type={type}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        disabled={disabled}
        className={`rs-in w-full rounded-lg border bg-gray-700 text-gray-100 placeholder-gray-500 px-3 py-2.5 text-sm transition-colors focus:outline-none focus:ring-2 focus:ring-blue-500/50 focus:border-blue-500 ${
          error ? 'border-red-500' : 'border-gray-600'
        } ${mono ? 'font-mono' : ''} ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`}
        aria-invalid={error ? true : undefined}
        aria-required={required || undefined}
        aria-describedby={error ? `${name}-error` : help ? `${name}-help` : undefined}
      />
      {error && (
        <p id={`${name}-error`} role="alert" className="text-red-400 text-xs mt-1">{error}</p>
      )}
      {help && !error && (
        <p id={`${name}-help`} className="text-gray-300 text-xs mt-1">{help}</p>
      )}
    </div>
  );
}

function FormSelect({
  label,
  name,
  value,
  onChange,
  options,
  help,
}: {
  label: string;
  name: string;
  value: string;
  onChange: (val: string) => void;
  options: { value: string; label: string }[];
  help?: string;
}) {
  return (
    <div>
      <label htmlFor={name} className="block text-sm font-medium text-gray-300 mb-1">{label}</label>
      <select
        id={name}
        name={name}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="rs-in w-full rounded-lg border border-gray-600 bg-gray-700 text-gray-100 px-3 py-2.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/50 focus:border-blue-500"
      >
        {options.map(o => (
          <option key={o.value} value={o.value}>{o.label}</option>
        ))}
      </select>
      {help && <p className="text-gray-300 text-xs mt-1">{help}</p>}
    </div>
  );
}

function ToggleSwitch({
  label,
  checked,
  onChange,
  help,
}: {
  label: string;
  checked: boolean;
  onChange: (val: boolean) => void;
  help?: string;
}) {
  return (
    <div className="flex items-start space-x-3">
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors flex-shrink-0 mt-0.5 ${
          checked ? 'bg-blue-600' : 'bg-gray-600'
        }`}
      >
        <span
          className={`inline-block h-4 w-4 rounded-full rs-knob transition-transform ${
            checked ? 'translate-x-6' : 'translate-x-1'
          }`}
        />
      </button>
      <div>
        <span className="text-sm font-medium text-gray-300">{label}</span>
        {help && <p className="text-gray-300 text-xs mt-0.5">{help}</p>}
      </div>
    </div>
  );
}

function SectionHeading({ title, description, color = 'blue' }: { title: string; description?: string; color?: string }) {
  const dotColors: Record<string, string> = {
    blue: 'bg-blue-400', green: 'bg-emerald-400', orange: 'bg-orange-400',
    purple: 'bg-purple-400', cyan: 'bg-cyan-400', red: 'bg-red-400',
  };
  return (
    <div className="mb-5">
      <h2 className="rs-heading text-lg font-semibold text-gray-100 flex items-center space-x-2">
        <span className={`w-2 h-2 rounded-full ${dotColors[color] ?? dotColors.blue}`} />
        <span>{title}</span>
      </h2>
      {description && <p className="text-sm text-gray-400 mt-1 ml-4">{description}</p>}
    </div>
  );
}

type TestConnectionState = 'idle' | 'loading' | 'success' | 'error';

/**
 * The connection test button: quiet while idle or testing, and its own success or failure
 * colours after a result. It is never styled as a destructive action.
 */
function testConnectionClass(state: TestConnectionState): string {
  const tone =
    state === 'success'
      ? 'bg-emerald-600/20 text-emerald-400 border border-emerald-600'
      : state === 'error'
        ? 'bg-red-600/20 text-red-400 border border-red-600'
        : 'rs-btn--quiet bg-gray-700 text-gray-300 border border-gray-600 hover:bg-gray-600';
  return `rs-btn px-4 py-2 rounded-lg text-sm font-medium transition-colors ${tone} disabled:opacity-50`;
}

function TestConnectionButton({ onTest }: { onTest: () => Promise<{ ok: boolean; message: string }> }) {
  const [state, setState] = useState<TestConnectionState>('idle');
  const [msg, setMsg] = useState('');

  const handleTest = async () => {
    setState('loading');
    try {
      const result = await onTest();
      setState(result.ok ? 'success' : 'error');
      setMsg(result.message);
    } catch (e: unknown) {
      setState('error');
      setMsg(e instanceof Error ? e.message : 'Connection test failed');
    }
    setTimeout(() => setState('idle'), 5000);
  };

  return (
    <div className="flex items-center space-x-3">
      <button
        onClick={handleTest}
        disabled={state === 'loading'}
        className={testConnectionClass(state)}
      >
        {state === 'loading' ? (
          <span className="flex items-center space-x-2">
            <svg className="w-4 h-4 animate-spin" viewBox="0 0 24 24" fill="none">
              <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
              <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
            </svg>
            <span>Testing...</span>
          </span>
        ) : state === 'success' ? (
          'Connected'
        ) : state === 'error' ? (
          'Failed'
        ) : (
          'Test Connection'
        )}
      </button>
      {msg && (state === 'success' || state === 'error') && (
        <span className={`text-xs ${state === 'success' ? 'text-emerald-400' : 'text-red-400'}`}>{msg}</span>
      )}
    </div>
  );
}

/** Marks fields the page collects but does not send today (#264, G12.4 and G12.6). */
function NotSentMarker({ detail }: { detail?: string }) {
  return (
    <p className="flex flex-wrap items-center gap-2 text-xs">
      <span className="rs-mark inline-flex items-center px-2 py-0.5 rounded border border-dashed border-orange-700/50 text-xs font-medium text-orange-300">
        Collected but not sent today
      </span>
      {detail && <span className="text-gray-400">{detail}</span>}
    </p>
  );
}

/**
 * Wraps the configuration steps. After a failed read of the saved configuration it shows the
 * failure with the client's error text and turns the form off until a read succeeds (#264, G12.10).
 * The fieldset is always rendered so the steps inside keep their state when the read settles.
 */
export function SavedConfigGate({
  read,
  onRetry,
  children,
}: {
  read: SavedConfigRead;
  onRetry: () => void;
  children: ReactNode;
}) {
  const failureId = useId();
  const { formOff } = savedConfigView(read.status);
  return (
    <>
      {read.status === 'failed' && (
        <div className="rs-bn rs-bn--danger mb-6 bg-red-900/30 border border-red-700 rounded-lg p-4 text-sm text-red-300">
          <div id={failureId} role="alert">
            <p className="font-semibold">The saved configuration could not be loaded</p>
            <p className="mt-1 break-words">{read.error}</p>
            <p className="mt-1">The form is turned off until the saved configuration is read.</p>
          </div>
          <div className="mt-3 flex flex-wrap items-center gap-3">
            <button
              type="button"
              onClick={onRetry}
              disabled={read.retrying}
              className="rs-btn rs-btn--quiet px-4 py-2 border border-gray-600 text-gray-300 hover:bg-gray-700 rounded-lg text-sm font-medium transition-colors disabled:opacity-50"
            >
              Retry
            </button>
            <span role="status" className="text-xs text-gray-400">
              {read.retrying ? 'Reading the saved configuration…' : ''}
            </span>
          </div>
        </div>
      )}
      <fieldset
        disabled={formOff}
        aria-describedby={formOff ? failureId : undefined}
        className="min-w-0 disabled:opacity-60"
      >
        {children}
      </fieldset>
    </>
  );
}

// ---------------------------------------------------------------------------
// Step 0: Welcome
// ---------------------------------------------------------------------------

function WelcomeStep() {
  return (
    <div className="text-center py-6">
      <div className="inline-flex items-center justify-center w-20 h-20 rounded-2xl bg-blue-600/10 border border-blue-500/30 mb-6">
        <svg className="w-10 h-10 text-blue-400" viewBox="0 0 48 48" fill="none">
          <rect x="2" y="2" width="44" height="44" rx="10" stroke="currentColor" strokeWidth="2.5" fill="none" />
          <path d="M14 24 L22 32 L34 16" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
          <circle cx="14" cy="16" r="3" fill="currentColor" opacity="0.5" />
          <circle cx="34" cy="32" r="3" fill="currentColor" opacity="0.5" />
        </svg>
      </div>
      <h2 className="rs-heading text-2xl font-bold text-gray-100 mb-3 font-display tracking-wide">Welcome to RepoSync</h2>
      <p className="text-gray-400 max-w-lg mx-auto leading-relaxed">
        This wizard will guide you through setting up bidirectional synchronization
        between your SVN repository and Git. At the end, you'll get a configuration
        file ready to deploy on your server.
      </p>
      <div className="mt-8 grid grid-cols-1 sm:grid-cols-3 gap-4 max-w-xl mx-auto text-left">
        <div className="bg-gray-700/50 rounded-lg p-4 border border-gray-600/50">
          <div className="text-blue-400 font-semibold text-sm mb-1">SVN & Git</div>
          <p className="text-gray-400 text-xs">Connect your SVN repo and Git provider (GitHub/Gitea)</p>
        </div>
        <div className="bg-gray-700/50 rounded-lg p-4 border border-gray-600/50">
          <div className="text-purple-400 font-semibold text-sm mb-1">Identity</div>
          <p className="text-gray-400 text-xs">Map SVN usernames to Git author identities</p>
        </div>
        <div className="bg-gray-700/50 rounded-lg p-4 border border-gray-600/50">
          <div className="text-emerald-400 font-semibold text-sm mb-1">Generate</div>
          <p className="text-gray-400 text-xs">Get a ready-to-deploy TOML configuration file</p>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 1: SVN
// ---------------------------------------------------------------------------

export function SvnStep({
  data,
  update,
  errors,
  svnPasswordSaved,
  svnPasswordDirty,
  onSvnPasswordChange,
}: {
  data: WizardData;
  update: (f: Partial<WizardData>) => void;
  errors: Errors;
  svnPasswordSaved: boolean;
  svnPasswordDirty: boolean;
  onSvnPasswordChange: () => void;
}) {
  const fieldId = useId();
  return (
    <div className="space-y-6">
      <SectionHeading
        title="SVN Repository"
        description="Configure the connection to your Subversion repository."
        color="orange"
      />
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="md:col-span-2">
          <FormField
            label="Repository URL"
            name="svn_url"
            value={data.svn_url}
            onChange={(v) => update({ svn_url: v })}
            error={errors.svn_url}
            placeholder="svn://svn.example.com/repos/project"
            help="The root URL of your SVN repository"
            mono
            required
          />
        </div>
        <FormField
          label="Username"
          name="svn_username"
          value={data.svn_username}
          onChange={(v) => update({ svn_username: v })}
          error={errors.svn_username}
          placeholder="sync-service"
          help="Service account username for SVN access"
          required
        />
        <div>
          <label className="block text-sm font-medium text-gray-300 mb-1" htmlFor={`${fieldId}-1`}>Password</label>
          <input
            type="password"
            value={data.svn_password}
            onChange={e => { update({ svn_password: e.target.value }); onSvnPasswordChange(); }}
            className="rs-in w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-100 text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
            placeholder={svnPasswordSaved && !svnPasswordDirty ? '\u2022\u2022\u2022\u2022\u2022\u2022\u2022 (saved)' : 'Enter SVN password'} id={`${fieldId}-1`}
          />
          <p className="text-xs text-gray-300 mt-1">Stored securely on the server — never written to config files</p>
        </div>
      </div>

      <div className="border-t border-gray-700 pt-5">
        <FormSelect
          label="Repository Layout"
          name="svn_layout"
          value={data.svn_layout}
          onChange={(v) => update({ svn_layout: v as 'standard' | 'custom' })}
          options={[
            { value: 'standard', label: 'Standard (trunk/branches/tags)' },
            { value: 'custom', label: 'Custom paths' },
          ]}
          help="Standard layout uses trunk/, branches/, and tags/ under the repo root"
        />
        <div className="mt-4 grid grid-cols-1 md:grid-cols-3 gap-4">
          <FormField
            label="Trunk Path"
            name="svn_trunk_path"
            value={data.svn_trunk_path}
            onChange={(v) => update({ svn_trunk_path: v })}
            error={errors.svn_trunk_path}
            mono
          />
          <FormField
            label="Branches Path"
            name="svn_branches_path"
            value={data.svn_branches_path}
            onChange={(v) => update({ svn_branches_path: v })}
            disabled={data.svn_layout === 'standard'}
            mono
          />
          <FormField
            label="Tags Path"
            name="svn_tags_path"
            value={data.svn_tags_path}
            onChange={(v) => update({ svn_tags_path: v })}
            disabled={data.svn_layout === 'standard'}
            mono
          />
        </div>
      </div>

      <div className="border-t border-gray-700 pt-5">
        <TestConnectionButton
          onTest={() => api.testSvnConnection({ url: data.svn_url, username: data.svn_username, password: data.svn_password || undefined })}
        />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 2: Git Provider
// ---------------------------------------------------------------------------

export function GitStep({
  data,
  update,
  errors,
  gitTokenSaved,
  gitTokenDirty,
  onGitTokenChange,
}: {
  data: WizardData;
  update: (f: Partial<WizardData>) => void;
  errors: Errors;
  gitTokenSaved: boolean;
  gitTokenDirty: boolean;
  onGitTokenChange: () => void;
}) {
  const fieldId = useId();
  const handleProviderChange = (provider: string) => {
    const p = provider as 'github' | 'gitea';
    const apiUrl = p === 'github' ? 'https://api.github.com' : data.git_api_url;
    update({ git_provider: p, git_api_url: apiUrl });
  };

  return (
    <div className="space-y-6">
      <SectionHeading
        title="Git Provider"
        description="Configure the connection to GitHub or Gitea."
        color="purple"
      />

      {/* Provider selection */}
      <div className="flex space-x-3">
        {(['github', 'gitea'] as const).map(p => (
          <button
            key={p}
            aria-pressed={data.git_provider === p}
            onClick={() => handleProviderChange(p)}
            className={`rs-btn flex-1 py-3 px-4 rounded-lg border text-sm font-medium transition-all ${
              data.git_provider === p
                ? 'rs-btn--primary border-blue-500 bg-blue-600/10 text-blue-400'
                : 'rs-btn--quiet border-gray-600 bg-gray-700/50 text-gray-400 hover:bg-gray-700'
            }`}
          >
            {p === 'github' ? 'GitHub / GitHub Enterprise' : 'Gitea'}
          </button>
        ))}
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="md:col-span-2">
          <FormField
            label="API URL"
            name="git_api_url"
            value={data.git_api_url}
            onChange={(v) => update({ git_api_url: v })}
            error={errors.git_api_url}
            placeholder={data.git_provider === 'github' ? 'https://api.github.com' : 'http://gitea.example.com:3000/api/v1'}
            help={data.git_provider === 'github' ? 'Use https://api.github.com for GitHub.com, or your GHE API URL' : 'Your Gitea server API URL (e.g., http://host:3000/api/v1)'}
            mono
            required
          />
        </div>
        <FormField
          label="Repository"
          name="git_repo"
          value={data.git_repo}
          onChange={(v) => update({ git_repo: v })}
          error={errors.git_repo}
          placeholder="owner/repo-name"
          help="In owner/repo format"
          mono
          required
        />
        <FormField
          label="Default Branch"
          name="git_default_branch"
          value={data.git_default_branch}
          onChange={(v) => update({ git_default_branch: v })}
          placeholder="main"
          mono
        />
        <div className="md:col-span-2">
          <div>
            <label className="block text-sm font-medium text-gray-300 mb-1" htmlFor={`${fieldId}-2`}>Personal Access Token</label>
            <input
              type="password"
              value={data.git_token}
              onChange={e => { update({ git_token: e.target.value }); onGitTokenChange(); }}
              className="rs-in w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-100 text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
              placeholder={gitTokenSaved && !gitTokenDirty ? '\u2022\u2022\u2022\u2022\u2022\u2022\u2022 (saved)' : 'ghp_xxxxxxxxxxxx or giteaToken'} id={`${fieldId}-2`}
            />
            <p className="text-xs text-gray-300 mt-1">Needs <code className="text-gray-400">repo</code> scope. Stored securely on the server.</p>
          </div>
        </div>
      </div>

      <div className="border-t border-gray-700 pt-5">
        <TestConnectionButton
          onTest={() => api.testGitConnection({ api_url: data.git_api_url, repo: data.git_repo, provider: data.git_provider })}
        />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 3: Sync Settings
// ---------------------------------------------------------------------------

export function SyncStep({
  data,
  update,
  errors,
}: {
  data: WizardData;
  update: (f: Partial<WizardData>) => void;
  errors: Errors;
}) {
  const fieldId = useId();
  const lfsThresholdError = errors.lfs_threshold_mb;
  return (
    <div className="space-y-6">
      <SectionHeading
        title="Sync Settings"
        description="Configure how commits are synchronized between SVN and Git."
        color="cyan"
      />

      {/* Mode selection */}
      <div>
        <p id={`${fieldId}-sync-mode`} className="block text-sm font-medium text-gray-300 mb-2">Sync Mode</p>
        <div role="group" aria-labelledby={`${fieldId}-sync-mode`} className="flex space-x-3">
          <button
            aria-pressed={data.sync_mode === 'direct'} onClick={() => update({ sync_mode: 'direct' })}
            className={`rs-btn rs-choice flex-1 py-3 px-4 rounded-lg border text-left transition-all ${
              data.sync_mode === 'direct'
                ? 'rs-btn--primary border-blue-500 bg-blue-600/10'
                : 'rs-btn--quiet border-gray-600 bg-gray-700/50 hover:bg-gray-700'
            }`}
          >
            <div className={`text-sm font-medium ${data.sync_mode === 'direct' ? 'text-blue-400' : 'text-gray-300'}`}>
              Direct Push
            </div>
            <p className="text-xs text-gray-400 mt-1">Commits are pushed directly to the target branch</p>
          </button>
          <button
            aria-pressed={data.sync_mode === 'pr'} onClick={() => update({ sync_mode: 'pr' })}
            className={`rs-btn rs-choice flex-1 py-3 px-4 rounded-lg border text-left transition-all ${
              data.sync_mode === 'pr'
                ? 'rs-btn--primary border-blue-500 bg-blue-600/10'
                : 'rs-btn--quiet border-gray-600 bg-gray-700/50 hover:bg-gray-700'
            }`}
          >
            <div className={`text-sm font-medium ${data.sync_mode === 'pr' ? 'text-blue-400' : 'text-gray-300'}`}>
              Pull Request
            </div>
            <p className="text-xs text-gray-400 mt-1">Changes create a PR for review before merging</p>
          </button>
        </div>
      </div>

      <div className="space-y-4">
        <ToggleSwitch
          label="Auto Merge"
          checked={data.sync_auto_merge}
          onChange={(v) => update({ sync_auto_merge: v })}
          help="Automatically resolve 3-way merge conflicts when possible"
        />
        <ToggleSwitch
          label="Sync Tags"
          checked={data.sync_tags}
          onChange={(v) => update({ sync_tags: v })}
          help="Synchronize SVN tags to Git tags and vice versa"
        />
      </div>

      {/* File Policy & LFS */}
      <div className="border-t border-gray-700 pt-5 space-y-4">
        <h3 className="rs-heading text-sm font-semibold text-gray-200">File Policy & Large File Storage (LFS)</h3>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="block text-sm font-medium text-gray-300 mb-1" htmlFor={`${fieldId}-3`}>LFS Threshold (MB)</label>
            <input
              type="number"
              min="0"
              value={data.lfs_threshold_mb}
              onChange={e => update({ lfs_threshold_mb: Number(e.target.value) || 0 })}
              className={`rs-in w-full px-3 py-2 bg-gray-700 border ${lfsThresholdError ? 'border-red-500' : 'border-gray-600'} rounded-lg text-gray-100 text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent`}
              placeholder="0 = disabled" id={`${fieldId}-3`}
              aria-invalid={lfsThresholdError ? true : undefined}
              aria-describedby={lfsThresholdError ? `${fieldId}-3-error` : `${fieldId}-3-help`}
            />
            {lfsThresholdError ? (
              <p id={`${fieldId}-3-error`} role="alert" className="text-red-400 text-xs mt-1">{lfsThresholdError}</p>
            ) : (
              <p id={`${fieldId}-3-help`} className="text-xs text-gray-400 mt-1">Files larger than this are tracked via Git LFS. Set 0 to disable.</p>
            )}
          </div>
          <div>
            <label className="block text-sm font-medium text-gray-300 mb-1" htmlFor={`${fieldId}-4`}>Max File Size (MB)</label>
            <input
              type="number"
              min="0"
              value={data.max_file_size_mb}
              onChange={e => update({ max_file_size_mb: Number(e.target.value) || 0 })}
              className="rs-in w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-100 text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
              placeholder="0 = no limit" id={`${fieldId}-4`}
            />
            <p className="text-xs text-gray-400 mt-1">Files larger than this are skipped entirely. Set 0 for no limit.</p>
          </div>
        </div>
        <FormField
          label="LFS Patterns"
          name="lfs_patterns"
          value={data.lfs_patterns}
          onChange={v => update({ lfs_patterns: v })}
          placeholder="*.psd, *.zip, *.bin, *.dll"
          help="Comma-separated glob patterns. Files matching these are always LFS-tracked regardless of size."
        />
        <FormField
          label="Ignore Patterns"
          name="ignore_patterns"
          value={data.ignore_patterns}
          onChange={v => update({ ignore_patterns: v })}
          placeholder="*.tmp, *.bak, .svn/"
          help="Comma-separated glob patterns. Matching files are excluded from sync."
        />
      </div>

      {/* PR-specific settings */}
      {data.sync_mode === 'pr' && (
        <div className="border-t border-gray-700 pt-5 space-y-4">
          <h3 className="rs-heading text-sm font-semibold text-gray-200">Pull Request Settings</h3>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <FormField
              label="PR Title Prefix"
              name="pr_title_prefix"
              value={data.pr_title_prefix}
              onChange={(v) => update({ pr_title_prefix: v })}
              placeholder="[svn-sync]"
            />
            <FormField
              label="Labels"
              name="pr_labels"
              value={data.pr_labels}
              onChange={(v) => update({ pr_labels: v })}
              placeholder="svn-sync, automated"
              help="Comma-separated list"
            />
            <FormField
              label="Reviewers"
              name="pr_reviewers"
              value={data.pr_reviewers}
              onChange={(v) => update({ pr_reviewers: v })}
              placeholder="username1, username2"
              help="Comma-separated GitHub usernames"
            />
            <div className="flex items-end">
              <ToggleSwitch
                label="Auto-merge PRs"
                checked={data.pr_auto_merge}
                onChange={(v) => update({ pr_auto_merge: v })}
                help="Automatically merge PRs when checks pass"
              />
            </div>
          </div>
          <NotSentMarker />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 4: Identity
// ---------------------------------------------------------------------------

export function IdentityStep({
  data,
  update,
  errors,
}: {
  data: WizardData;
  update: (f: Partial<WizardData>) => void;
  errors: Errors;
}) {
  const [newMapping, setNewMapping] = useState<AuthorMapping>({ svn_username: '', name: '', email: '' });

  const addMapping = () => {
    if (!newMapping.svn_username || !newMapping.name || !newMapping.email) return;
    update({ identity_mappings: [...data.identity_mappings, { ...newMapping }] });
    setNewMapping({ svn_username: '', name: '', email: '' });
  };

  const removeMapping = (svnUsername: string) => {
    update({ identity_mappings: data.identity_mappings.filter(m => m.svn_username !== svnUsername) });
  };

  return (
    <div className="space-y-6">
      <SectionHeading
        title="Identity Mapping"
        description="Map SVN usernames to Git author identities so commits are attributed correctly."
        color="green"
      />

      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <FormField
          label="Fallback Email Domain"
          name="identity_email_domain"
          value={data.identity_email_domain}
          onChange={(v) => update({ identity_email_domain: v })}
          error={errors.identity_email_domain}
          placeholder="company.com"
          help="Unmapped users get email: svn-username@domain"
        />
        <FormField
          label="Author Mapping File"
          name="identity_mapping_file"
          value={data.identity_mapping_file}
          onChange={(v) => update({ identity_mapping_file: v })}
          placeholder="Leave blank to use the configured file"
          help="Optional. Mappings below are saved to identity.mapping_file from the server configuration (or identity-mappings.toml beside it), keeping entries already there. A different path here is not used; the server reports it as a warning."
          mono
        />
      </div>

      {/* Inline mappings table */}
      <div className="border-t border-gray-700 pt-5">
        <h3 className="rs-heading text-sm font-semibold text-gray-200 mb-3">Inline Author Mappings</h3>
        {data.identity_mappings.length > 0 && (
          <div className="overflow-x-auto mb-4">
            <table className="rs-tbl min-w-full divide-y divide-gray-700">
              <thead>
                <tr>
                  <th className="px-3 py-2 text-left text-xs font-medium text-gray-400 uppercase">SVN Username</th>
                  <th className="px-3 py-2 text-left text-xs font-medium text-gray-400 uppercase">Git Name</th>
                  <th className="px-3 py-2 text-left text-xs font-medium text-gray-400 uppercase">Email</th>
                  <th className="px-3 py-2 w-16"></th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-700">
                {data.identity_mappings.map(m => (
                  <tr key={m.svn_username} className="hover:bg-gray-700/50">
                    <td className="px-3 py-2 text-sm font-mono text-gray-200">{m.svn_username}</td>
                    <td className="px-3 py-2 text-sm text-gray-300">{m.name}</td>
                    <td className="px-3 py-2 text-sm text-gray-300">{m.email}</td>
                    <td className="px-3 py-2 text-right">
                      <button onClick={() => removeMapping(m.svn_username)} className="text-red-400 hover:text-red-300 text-xs">
                        Remove
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <div className="flex space-x-2">
          <input
            aria-label="Mapping SVN username" placeholder="SVN username"
            value={newMapping.svn_username}
            onChange={(e) => setNewMapping({ ...newMapping, svn_username: e.target.value })}
            className="rs-in flex-1 rounded-lg border border-gray-600 bg-gray-700 text-gray-100 placeholder-gray-500 px-3 py-2 text-sm focus:ring-2 focus:ring-blue-500/50 focus:border-blue-500 focus:outline-none font-mono"
          />
          <input
            aria-label="Mapping full name" placeholder="Full Name"
            value={newMapping.name}
            onChange={(e) => setNewMapping({ ...newMapping, name: e.target.value })}
            className="rs-in flex-1 rounded-lg border border-gray-600 bg-gray-700 text-gray-100 placeholder-gray-500 px-3 py-2 text-sm focus:ring-2 focus:ring-blue-500/50 focus:border-blue-500 focus:outline-none"
          />
          <input
            aria-label="Mapping email" placeholder="email@company.com"
            value={newMapping.email}
            onChange={(e) => setNewMapping({ ...newMapping, email: e.target.value })}
            className="rs-in flex-1 rounded-lg border border-gray-600 bg-gray-700 text-gray-100 placeholder-gray-500 px-3 py-2 text-sm focus:ring-2 focus:ring-blue-500/50 focus:border-blue-500 focus:outline-none"
          />
          <button
            onClick={addMapping}
            disabled={!newMapping.svn_username || !newMapping.name || !newMapping.email}
            className="rs-btn rs-btn--primary px-4 py-2 bg-blue-600 rs-text-on-accent rounded-lg text-sm font-medium hover:bg-blue-700 disabled:opacity-50 transition-colors"
          >
            Add
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 5: Server & Auth
// ---------------------------------------------------------------------------

export function ServerAuthStep({
  data,
  update,
  errors,
}: {
  data: WizardData;
  update: (f: Partial<WizardData>) => void;
  errors: Errors;
}) {
  return (
    <div className="space-y-8">
      {/* Web Server */}
      <div>
        <SectionHeading title="Web Server" description="Configure the dashboard web server." color="cyan" />
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <FormField
            label="Listen Address"
            name="web_listen"
            value={data.web_listen}
            onChange={(v) => update({ web_listen: v })}
            error={errors.web_listen}
            placeholder="0.0.0.0:8080"
            help="host:port to bind the web server"
            mono
            required
          />
          <FormSelect
            label="Authentication Mode"
            name="web_auth_mode"
            value={data.web_auth_mode}
            onChange={(v) => update({ web_auth_mode: v as 'simple' | 'github_oauth' | 'both' })}
            options={[
              { value: 'simple', label: 'Simple (password)' },
              { value: 'github_oauth', label: 'GitHub OAuth' },
              { value: 'both', label: 'Both' },
            ]}
          />
        </div>
        {/* Auth-specific fields */}
        {(data.web_auth_mode === 'simple' || data.web_auth_mode === 'both') && (
          <div className="mt-4">
            <FormField
              label="Admin Password Env Var"
              name="web_admin_password_env"
              value={data.web_admin_password_env}
              onChange={(v) => update({ web_admin_password_env: v })}
              error={errors.web_admin_password_env}
              placeholder="REPOSYNC_ADMIN_PASSWORD"
              mono
              required
            />
          </div>
        )}
        {(data.web_auth_mode === 'github_oauth' || data.web_auth_mode === 'both') && (
          <div className="mt-4 grid grid-cols-1 md:grid-cols-2 gap-4">
            <FormField
              label="OAuth Client ID"
              name="web_oauth_client_id"
              value={data.web_oauth_client_id}
              onChange={(v) => update({ web_oauth_client_id: v })}
              mono
            />
            <FormField
              label="OAuth Client Secret Env Var"
              name="web_oauth_client_secret_env"
              value={data.web_oauth_client_secret_env}
              onChange={(v) => update({ web_oauth_client_secret_env: v })}
              mono
            />
            <div className="md:col-span-2">
              <FormField
                label="Allowed GitHub Users"
                name="web_oauth_allowed_users"
                value={data.web_oauth_allowed_users}
                onChange={(v) => update({ web_oauth_allowed_users: v })}
                placeholder="user1, user2"
                help="Comma-separated list of GitHub usernames"
              />
            </div>
          </div>
        )}
        <div className="mt-4">
          <NotSentMarker detail="Authentication mode and the OAuth fields" />
        </div>
      </div>

      {/* Daemon */}
      <div className="border-t border-gray-700 pt-6">
        <SectionHeading title="Daemon Settings" description="Configure the sync daemon process." color="green" />
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <FormField
            label="Poll Interval (seconds)"
            name="daemon_poll_interval"
            value={data.daemon_poll_interval}
            onChange={(v) => update({ daemon_poll_interval: parseInt(v) || 0 })}
            error={errors.daemon_poll_interval}
            type="number"
            required
          />
          <FormSelect
            label="Log Level"
            name="daemon_log_level"
            value={data.daemon_log_level}
            onChange={(v) => update({ daemon_log_level: v })}
            options={[
              { value: 'trace', label: 'Trace' },
              { value: 'debug', label: 'Debug' },
              { value: 'info', label: 'Info' },
              { value: 'warn', label: 'Warn' },
              { value: 'error', label: 'Error' },
            ]}
          />
          <FormField
            label="Data Directory"
            name="daemon_data_dir"
            value={data.daemon_data_dir}
            onChange={(v) => update({ daemon_data_dir: v })}
            error={errors.daemon_data_dir}
            placeholder="/var/lib/reposync"
            mono
            required
          />
        </div>
      </div>

      {/* Notifications */}
      <div className="border-t border-gray-700 pt-6">
        <SectionHeading title="Notifications" description="Optional: configure Slack or email alerts." color="orange" />
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div className="md:col-span-2">
            <FormField
              label="Slack Webhook Env Var"
              name="notif_slack_webhook_env"
              value={data.notif_slack_webhook_env}
              onChange={(v) => update({ notif_slack_webhook_env: v })}
              placeholder="REPOSYNC_SLACK_WEBHOOK"
              help="Leave empty to disable Slack notifications"
              mono
            />
          </div>
          <FormField
            label="Email SMTP Server"
            name="notif_email_smtp"
            value={data.notif_email_smtp}
            onChange={(v) => update({ notif_email_smtp: v })}
            placeholder="smtp.company.com:587"
            mono
          />
          <FormField
            label="From Address"
            name="notif_email_from"
            value={data.notif_email_from}
            onChange={(v) => update({ notif_email_from: v })}
            placeholder="reposync@company.com"
          />
          <div className="md:col-span-2">
            <FormField
              label="Email Recipients"
              name="notif_email_recipients"
              value={data.notif_email_recipients}
              onChange={(v) => update({ notif_email_recipients: v })}
              placeholder="admin@company.com, ops@company.com"
              help="Comma-separated list"
            />
          </div>
        </div>
        <div className="mt-4">
          <NotSentMarker />
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 6: Review & Save
// ---------------------------------------------------------------------------

export function ReviewStep({
  data,
  configSaved,
  saveError,
  saveWarnings,
  onSave,
  readStatus,
}: {
  data: WizardData;
  configSaved: boolean;
  saveError: string;
  saveWarnings: string[];
  onSave: () => void;
  readStatus: SavedConfigRead['status'];
}) {
  const [saving, setSaving] = useState(false);
  const { canSave } = savedConfigView(readStatus);

  const handleSave = async () => {
    if (!canSave) return;
    setSaving(true);
    await onSave();
    setSaving(false);
  };

  return (
    <div className="space-y-6">
      <SectionHeading title="Review Configuration" description="Review your settings and save to the server." color="blue" />

      {/* Summary cards */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <SummaryCard
          title="SVN"
          color="orange"
          items={[
            ['URL', data.svn_url],
            ['Username', data.svn_username],
            ['Layout', data.svn_layout],
            ['Trunk', data.svn_trunk_path],
          ]}
        />
        <SummaryCard
          title="Git"
          color="purple"
          items={[
            ['Provider', data.git_provider],
            ['API URL', data.git_api_url],
            ['Repository', data.git_repo],
            ['Branch', data.git_default_branch],
          ]}
        />
        <SummaryCard
          title="Sync"
          color="cyan"
          items={[
            ['Mode', data.sync_mode],
            ['Auto Merge', data.sync_auto_merge ? 'Yes' : 'No'],
            ['Sync Tags', data.sync_tags ? 'Yes' : 'No'],
            ...(data.lfs_threshold_mb > 0 ? [['LFS Threshold', `${data.lfs_threshold_mb} MB`] as [string, string]] : []),
            ...(data.max_file_size_mb > 0 ? [['Max File Size', `${data.max_file_size_mb} MB`] as [string, string]] : []),
          ]}
        />
        <SummaryCard
          title="Server"
          color="green"
          items={[
            ['Listen', data.web_listen],
            ['Auth', data.web_auth_mode],
            ['Poll Interval', `${data.daemon_poll_interval}s`],
            ['Log Level', data.daemon_log_level],
          ]}
        />
      </div>

      {data.identity_mappings.length > 0 && (
        <div className="bg-gray-700/30 rounded-lg p-4 border border-gray-700">
          <span className="text-sm text-gray-300">
            {data.identity_mappings.length} identity mapping{data.identity_mappings.length !== 1 ? 's' : ''} will be saved
          </span>
        </div>
      )}

      {/* Save button */}
      <div className="border-t border-gray-700 pt-5">
        {saveError && (
          <div className="rs-bn rs-bn--danger mb-4 bg-red-900/30 border border-red-700 rounded-lg p-4 text-sm text-red-300">
            <span className="font-semibold">Error: </span>{saveError}
          </div>
        )}
        {saveWarnings.length > 0 && (
          <div className="rs-bn rs-bn--warn mb-4 bg-yellow-900/30 border border-yellow-700 rounded-lg p-4 text-sm text-yellow-300">
            <span className="font-semibold">Warnings:</span>
            <ul className="list-disc list-inside mt-1">
              {saveWarnings.map((w, i) => <li key={i}>{w}</li>)}
            </ul>
          </div>
        )}
        {configSaved ? (
          <div className="rs-bn rs-bn--ok bg-emerald-900/30 border border-emerald-700 rounded-lg p-5 text-center">
            <div className="text-3xl mb-2">✓</div>
            <h3 className="rs-heading text-lg font-semibold text-emerald-300">Configuration Saved</h3>
            <p className="text-sm text-gray-400 mt-1">Click "Next: Import History" to import your SVN repository.</p>
          </div>
        ) : (
          <>
            {readStatus === 'reading' && (
              <p role="status" className="mb-4 text-sm text-gray-400">Waiting for the saved configuration to be read.</p>
            )}
            <button
              onClick={handleSave}
              disabled={saving || !canSave}
              className="rs-btn rs-btn--primary w-full py-3 bg-blue-600 rs-text-on-accent hover:bg-blue-700 rounded-lg text-sm font-medium transition-colors shadow-sm disabled:opacity-50"
            >
              {saving ? (
                <span className="flex items-center justify-center space-x-2">
                  <svg className="animate-spin h-4 w-4" viewBox="0 0 24 24"><circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" fill="none" /><path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" /></svg>
                  <span>Saving Configuration...</span>
                </span>
              ) : 'Save Configuration to Server'}
            </button>
          </>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 7: Import
// ---------------------------------------------------------------------------

function WelcomeImportCheck({ onStatus }: { onStatus: (status: ImportStatus) => void }) {
  const { status, statusError } = useSetupImport();
  useEffect(() => {
    if (status && !statusError) onStatus(status);
  }, [onStatus, status, statusError]);
  return statusError ? <p role="alert" className="mb-4 text-yellow-300">{statusError} Checking again automatically.</p> : null;
}

function ImportStep() {
  const navigate = useNavigate();
  const { status, statusError, actionError, actionMessage, starting, cancelling, awaitingStatus, start, cancel } = useSetupImport();
  const actionsDisabled = starting || cancelling || awaitingStatus || !!statusError;
  const logRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = useState(true);

  // Auto-scroll log
  useEffect(() => {
    if (autoScroll && logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [status?.log_lines, autoScroll]);

  const handleLogScroll = () => {
    if (!logRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = logRef.current;
    setAutoScroll(scrollHeight - scrollTop - clientHeight < 50);
  };

  const identified = !!(status?.operation_id && status.lifecycle);
  const lifecycle = identified ? status?.lifecycle : undefined;
  const needsReconciliation = lifecycle === 'reconciliation_required';
  const isComplete = lifecycle === 'completed';
  const isFailed = lifecycle === 'failed';
  const isCancelled = lifecycle === 'cancelled';
  const isTerminal = isComplete || isFailed || isCancelled || needsReconciliation;
  const isCancelling = lifecycle === 'cancel_requested' || lifecycle === 'cancelling';
  const reportedPhase = status?.phase ?? 'idle';
  const hasWorkerPhase = ['connecting', 'importing', 'verifying', 'final_push'].includes(reportedPhase);
  const isActive = !isTerminal && (hasWorkerPhase || status?.busy === true ||
    lifecycle === 'queued' || lifecycle === 'running' || isCancelling);
  const startRepository = status?.start_repository_id;
  const startName = status?.start_repository_name || startRepository;
  const changedRepository = status?.operation_repository_id && status.operation_repository_id !== startRepository;
  const isLegacyOutcome = !identified && ['completed', 'cancelled', 'failed'].includes(reportedPhase);
  // The journal owns outcomes. The worker supplies only the current execution phase.
  const phase: ImportStatus['phase'] = isComplete ? 'completed' : isCancelled ? 'cancelled'
    : isFailed || needsReconciliation ? 'failed' : hasWorkerPhase ? reportedPhase : 'idle';
  // Elapsed time for push phase
  const pushElapsed = (() => {
    if (!status?.push_started_at) return '';
    const start = new Date(status.push_started_at).getTime();
    const secs = Math.max(0, Math.floor((Date.now() - start) / 1000));
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return m > 0 ? `${m}m ${s}s` : `${s}s`;
  })();

  return (
    <div className="space-y-6">
      <SectionHeading
        title="Import SVN History"
        description="Import your entire SVN repository as individual Git commits with proper author attribution."
        color="blue"
      />

      {statusError && <p role="alert" className="text-yellow-300">{statusError} {status && 'Showing the last reported status.'}</p>}
      {actionError && <p role="alert" className="text-red-300">{actionError}</p>}
      {actionMessage && <p role="status" className="text-blue-300">{actionMessage}</p>}
      {!status && !statusError && <p role="status">Reading import status…</p>}
      {status && !isLegacyOutcome && !status.historical_phase && !needsReconciliation && <ImportPhaseGraphic currentPhase={phase} />}
      {status?.historical_phase && (
        <p>Interrupted setup history. The previous session last reported “{importPhaseLabel(status.historical_phase)}”.
          Its final outcome and publication are unknown.</p>
      )}
      {isLegacyOutcome && <p>Legacy import progress reported “{reportedPhase}”. A durable operation outcome and publication checkpoint are unavailable.</p>}
      {status?.start_blocked_reason && !canStartSetupImport(status) && <p className="text-yellow-300">{status.start_blocked_reason}</p>}
      {status && !status.start_blocked_reason && !isLegacyOutcome && !isActive && !isTerminal && !canStartSetupImport(status) &&
        <p>The server has not made a new setup import available.</p>}

      {canStartSetupImport(status) && startRepository && (
        <div className="text-center py-4">
          <button
            onClick={() => { void start(startRepository); }}
            disabled={actionsDisabled}
            className="rs-btn rs-btn--primary px-8 py-3 bg-blue-600 rs-text-on-accent hover:bg-blue-700 rounded-lg text-base font-medium transition-colors shadow-lg disabled:opacity-50"
          >
            {starting ? 'Starting import…' : changedRepository ? `Start import for ${startName}`
              : isFailed ? 'Retry Import' : isCancelled ? 'Restart Import' : 'Start Full Import'}
          </button>
          <p className="text-xs text-gray-300 mt-3">This will import all SVN revisions for {startName} as individual Git commits.</p>
        </div>
      )}

      {status && (isActive || isTerminal) && (
        <div className="space-y-4">
          <ImportProgressMeter status={{ ...status, phase }} name="Setup import" />
          <p className="text-sm text-gray-200">{lifecycle === 'running' && phase === 'idle' ? 'Running' : importStateLabel({ phase, lifecycle })}</p>
          {status.operation_id && <p className="text-xs text-gray-400 font-mono">Operation {status.operation_id}</p>}
          {isTerminal && status.busy && <p>Outcome recorded. The repository is still busy.</p>}
          {identified && (
            <dl className="grid gap-3 sm:grid-cols-2">
              <ImportCheckpoint label="Local progress" revision={status.last_local_svn_rev} sha={status.last_local_git_sha} />
              <ImportCheckpoint label="Confirmed publication" revision={status.last_confirmed_svn_rev} sha={status.last_confirmed_git_sha} />
            </dl>
          )}

          {/* Stats row */}
          {status.has_worker_progress !== false && (
            <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
              <StatCard label="Revisions" value={`${status.current_rev ?? 0} / ${status.total_revs ?? 0}`} />
              <StatCard label="Commits Created" value={String(status.commits_created ?? 0)} />
              <StatCard label="Current Files" value={String(status.current_file_count ?? 0)} />
              <StatCard label="LFS Files" value={String(status.lfs_unique_count ?? 0)} />
              <StatCard label="Batches Pushed" value={String(status.batches_pushed ?? 0)} />
            </div>
          )}

          {/* Verification status */}
          {phase === 'verifying' && (
            <div className="rs-bn rs-bn--info bg-blue-900/20 border border-blue-700 rounded-lg p-4 flex items-center space-x-3">
              <svg className="w-5 h-5 text-blue-400 animate-spin flex-shrink-0" viewBox="0 0 24 24" fill="none">
                <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
              </svg>
              <div>
                <p className="text-sm font-medium text-blue-300">Verifying import integrity...</p>
                <p className="text-xs text-gray-400 mt-0.5">Comparing file trees between SVN and Git to ensure completeness.</p>
              </div>
            </div>
          )}

          {/* Final push progress */}
          {phase === 'final_push' && (
            <div className="rs-bn rs-bn--git bg-purple-900/20 border border-purple-700 rounded-lg p-4 flex items-center space-x-3">
              <svg className="w-5 h-5 text-purple-400 animate-spin flex-shrink-0" viewBox="0 0 24 24" fill="none">
                <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
              </svg>
              <div>
                <p className="text-sm font-medium text-purple-300">Pushing final commits to remote...</p>
                {pushElapsed && (
                  <p className="text-xs text-gray-400 mt-0.5">Push running for {pushElapsed}</p>
                )}
              </div>
            </div>
          )}

          {isActive && (
            <div className="text-center space-y-2">
              {isCancelling ? (
                <p className="text-sm text-yellow-300">
                  Stop requested. Waiting for the worker to finish. Published commits remain in place.
                </p>
              ) : identified && canCancelSetupImport(status) && status.operation_id ? (
                <button
                  onClick={() => { if (status.operation_id) void cancel(status.operation_id); }}
                  disabled={actionsDisabled}
                  className="rs-btn rs-btn--quiet px-4 py-2 border border-red-600 text-red-400 hover:bg-red-900/30 rounded-lg text-sm transition-colors disabled:opacity-50"
                >
                  Cancel Import
                </button>
              ) : (
                <p>This import is read-only here. {identified ? 'The setup worker cannot confirm it can stop this operation.' : 'The server has not identified a setup operation to stop.'}</p>
              )}
            </div>
          )}
        </div>
      )}

      {/* Operation log */}
      <div className="relative">
        <div className="flex items-center justify-between mb-2">
          <h3 className="rs-heading text-sm font-semibold text-gray-300">Operation Log</h3>
          {!autoScroll && (
            <button
              onClick={() => {
                setAutoScroll(true);
                if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
              }}
              className="text-xs text-blue-400 hover:text-blue-300 transition-colors"
            >
              ↓ Scroll to bottom
            </button>
          )}
        </div>
        <div
          ref={logRef}
          aria-label="Import operation log" aria-live="off"
          onScroll={handleLogScroll}
          className="bg-gray-950 border border-gray-700 rounded-lg p-4 h-[400px] overflow-y-auto font-mono text-sm leading-relaxed"
        >
          {(!status?.log_lines || status.log_lines.length === 0) ? (
            <p className="text-gray-400 italic">No worker log reported.</p>
          ) : (
            status.log_lines.map((line, i) => (
              <div key={i} className={`whitespace-pre-wrap ${getLogLineColor(line)}`}>
                {line}
              </div>
            ))
          )}
        </div>
      </div>

      {/* Server Monitor — visible during active import */}
      {(isActive || isComplete || isFailed) && <ServerMonitor />}

      {(isFailed || needsReconciliation) && (
        <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-5">
          <h3 className="rs-heading text-sm font-semibold text-red-300 mb-2">{needsReconciliation ? 'Reconciliation required' : 'Import failed'}</h3>
          {needsReconciliation && <p>Review the recorded outcome and confirm the published Git history before starting another import.</p>}
          {status?.outcome_detail && <p>{status.outcome_detail}</p>}
          {status?.errors.filter(error => error !== status.outcome_detail).map((err, i) => (
            <p key={i} className="text-sm text-red-200">{err}</p>
          ))}
        </div>
      )}

      {/* Success state */}
      {isComplete && (
        <div className="rs-bn rs-bn--ok bg-emerald-900/30 border border-emerald-700 rounded-lg p-6 text-center">
          <div className="text-4xl mb-3">✓</div>
          <h3 className="rs-heading text-xl font-semibold text-emerald-300">{status?.busy ? 'Completion recorded' : 'Import Complete'}</h3>
          {status?.has_worker_progress !== false && <p className="text-gray-400 mt-2">
            {status?.commits_created} commit{status?.commits_created !== 1 ? 's' : ''} imported from {status?.total_revs} SVN revision{status?.total_revs !== 1 ? 's' : ''}
          </p>}
          {(status?.lfs_unique_count ?? 0) > 0 && (
            <p className="text-gray-300 text-sm mt-1">{status?.lfs_unique_count} unique files tracked via Git LFS</p>
          )}
          {status?.verification && (
            <div className={`mt-3 text-sm ${status.verification.ok ? 'text-emerald-400' : 'text-yellow-400'}`}>
              {status.verification.message}
            </div>
          )}
          <button
            onClick={() => navigate('/login')}
            className="rs-btn rs-btn--state mt-4 px-8 py-3 bg-emerald-600 rs-text-on-state hover:bg-emerald-700 rounded-lg text-base font-medium transition-colors shadow-lg"
          >
            Go to Dashboard →
          </button>
        </div>
      )}

      {/* Cancelled state */}
      {isCancelled && (
        <div className="rs-bn rs-bn--warn bg-yellow-900/30 border border-yellow-700 rounded-lg p-5 text-center">
          <h3 className="rs-heading text-lg font-semibold text-yellow-300">{status?.busy ? 'Cancellation recorded' : 'Import Cancelled'}</h3>
          <p className="text-gray-400 mt-1">Published commits remain in place.</p>
          <div className="flex justify-center space-x-3 mt-3">
            <button
              onClick={() => navigate('/login')}
              className="rs-btn rs-btn--quiet px-4 py-2 border border-gray-600 text-gray-300 hover:bg-gray-700 rounded-lg text-sm transition-colors"
            >
              Go to Dashboard
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function ImportCheckpoint({ label, revision, sha }: { label: string; revision?: number | null; sha?: string | null }) {
  return (
    <div className="rounded-lg border border-gray-700 p-3">
      <dt className="text-sm font-semibold">{label}</dt>
      <dd className="break-all text-sm text-gray-300">
        {revision != null && <span>SVN r{revision} </span>}
        {sha && <code>{sha}</code>}
        {revision == null && !sha && 'No checkpoint reported'}
      </dd>
    </div>
  );
}

function getLogLineColor(line: string): string {
  // Log lines are now prefixed with [HH:MM:SS] by the backend,
  // so strip the timestamp before checking the tag.
  const stripped = line.replace(/^\[\d{2}:\d{2}:\d{2}\]\s*/, '');
  if (stripped.startsWith('[push]')) return 'text-cyan-400';
  if (stripped.startsWith('[ok]')) return 'text-emerald-400';
  if (stripped.startsWith('[error]')) return 'text-red-400';
  if (stripped.startsWith('[warn]')) return 'text-yellow-400';
  if (stripped.startsWith('[skip]')) return 'text-gray-300';
  if (stripped.startsWith('[info]')) return 'text-blue-400';
  return 'text-gray-400';
}

function StatCard({ label, value }: { label: string; value: string }) {
  return (
    <div className="bg-gray-700/30 rounded-lg p-3 border border-gray-700 text-center">
      <div className="text-lg font-bold text-gray-100 font-mono">{value}</div>
      <div className="text-xs text-gray-300 mt-0.5">{label}</div>
    </div>
  );
}

function SummaryCard({ title, color, items }: { title: string; color: string; items: [string, string][] }) {
  const borderColors: Record<string, string> = {
    orange: 'border-orange-700/50', purple: 'border-purple-700/50',
    cyan: 'border-cyan-700/50', green: 'border-emerald-700/50',
    blue: 'border-blue-700/50',
  };
  const titleColors: Record<string, string> = {
    orange: 'text-orange-400', purple: 'text-purple-400',
    cyan: 'text-cyan-400', green: 'text-emerald-400',
    blue: 'text-blue-400',
  };

  return (
    <div className={`bg-gray-700/30 rounded-lg p-4 border ${borderColors[color] ?? 'border-gray-700'}`}>
      <h4 className={`text-sm font-semibold mb-2 ${titleColors[color] ?? 'text-gray-300'}`}>{title}</h4>
      <dl className="space-y-1.5">
        {items.map(([label, value]) => (
          <div key={label} className="flex justify-between text-xs">
            <dt className="text-gray-300">{label}</dt>
            <dd className="text-gray-300 font-mono truncate ml-3 max-w-[60%] text-right">{value || '-'}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
