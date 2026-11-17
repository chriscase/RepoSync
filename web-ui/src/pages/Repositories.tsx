import { useId } from 'react';
import Dialog from '../components/Dialog';
import { clientSession } from '../session';
import RepositoryStatusSummary from '../components/StatusObservation';
import { useRepositorySave } from '../useRepositorySave';
import { samePublicConfiguration, retainUnsavedCredentials, sameImportPreferences, canContinueWithPreferences, repositoryEditorKey, publicConfiguration } from '../repositorySave';
import RepositorySaveNotice from '../components/RepositorySaveNotice';
import { useEffect, useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useLocation, useNavigate } from 'react-router-dom';
import { api, type Repository } from '../api';
import {
  DEFAULT_REPO_IMPORT_MODE,
  type RepoImportMode,
} from '../importBaseline';
import ImportModeFields from '../components/ImportModeFields';
import { readBranchPairRemovalNotice, readPersistedBranchPairRemovalNotice } from '../branchPairRemoval';
import BranchPairRemovalNotice from '../components/BranchPairRemovalNotice';
import ManagedRemovalReceiptNotice from '../components/ManagedRemovalReceiptNotice';
import { readAllManagedRemovalReceipts, shouldDisplayManagedRemovalReceipt } from '../managedRemoval';
import { GitBranch, Plus, Database, Clock, X, ArrowRight, CheckCircle2, XCircle } from 'lucide-react';
import { getStoredUser } from '../utils/auth';
import { useText } from '../layouts/vocabulary';

/** Custom SVG icon: two circular sync arrows, one blue (SVN) and one purple (Git) */
function SyncArrowsIcon({ className = 'w-8 h-8' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 32 32" fill="none" xmlns="http://www.w3.org/2000/svg">
      {/* Blue (SVN) arrow - top arc going right */}
      <path
        d="M8 14 A8 8 0 0 1 24 14"
        style={{ stroke: 'var(--rs-svn, #3b82f6)' }}
        strokeWidth="2.5"
        strokeLinecap="round"
        fill="none"
      />
      <path d="M22 10 L24 14 L20 14" style={{ fill: 'var(--rs-svn, #3b82f6)' }} />
      {/* Purple (Git) arrow - bottom arc going left */}
      <path
        d="M24 18 A8 8 0 0 1 8 18"
        style={{ stroke: 'var(--rs-git, #a855f7)' }}
        strokeWidth="2.5"
        strokeLinecap="round"
        fill="none"
      />
      <path d="M10 22 L8 18 L12 18" style={{ fill: 'var(--rs-git, #a855f7)' }} />
    </svg>
  );
}

/** A connection test's outcome as the dialog shows it: the client's message and when it arrived. */
export interface ConnectionTestOutcome {
  ok: boolean;
  message: string;
  testedAt: Date;
}

function twoDigits(value: number): string {
  return String(value).padStart(2, '0');
}

/** Local wall-clock time of a test result, hours:minutes:seconds. */
function testedAtLabel(at: Date): string {
  return `${twoDigits(at.getHours())}:${twoDigits(at.getMinutes())}:${twoDigits(at.getSeconds())}`;
}

/**
 * The result beside a "Test … Connection" button. The live region is always in the page so a
 * new result is announced; it is empty while idle and while a test runs. The result carries a
 * mark (with its meaning in words for assistive technology) and the time it arrived.
 */
export function ConnectionTestResult({ result }: { result: ConnectionTestOutcome | null }) {
  return (
    <span role="status">
      {result && (
        <span className={`ml-3 text-xs ${result.ok ? 'text-green-300' : 'text-red-300'}`}>
          {result.ok ? (
            <CheckCircle2 className="inline-block w-3.5 h-3.5 mr-1 align-text-bottom" aria-hidden="true" />
          ) : (
            <XCircle className="inline-block w-3.5 h-3.5 mr-1 align-text-bottom" aria-hidden="true" />
          )}
          <span className="sr-only">{result.ok ? 'Connection test passed: ' : 'Connection test failed: '}</span>
          {result.message}
          <span className="text-gray-400">
            {' · tested '}
            <time dateTime={result.testedAt.toISOString()}>{testedAtLabel(result.testedAt)}</time>
          </span>
        </span>
      )}
    </span>
  );
}

const inputClass =
  'rs-in w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

const selectClass =
  'rs-in w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

interface AddRepoForm {
  name: string;
  svn_url: string;
  svn_branch: string;
  svn_username: string;
  svn_password: string;
  git_provider: string;
  git_api_url: string;
  git_repo: string;
  git_branch: string;
  git_token: string;
  sync_mode: string;
  poll_interval_secs: number;
  lfs_threshold_mb: number;
  auto_merge: boolean;
  enabled: boolean;
  initial_import_mode: RepoImportMode;
  initial_svn_revision: string;
}

const defaultForm: AddRepoForm = {
  name: '',
  svn_url: '',
  svn_branch: 'trunk',
  svn_username: '',
  svn_password: '',
  git_provider: 'github',
  git_api_url: 'https://api.github.com',
  git_repo: '',
  git_branch: 'main',
  git_token: '',
  sync_mode: 'direct',
  poll_interval_secs: 300,
  lfs_threshold_mb: 0,
  auto_merge: false,
  enabled: true,
  initial_import_mode: DEFAULT_REPO_IMPORT_MODE,
  initial_svn_revision: 'HEAD',
};

export default function Repositories() {
  const key=repositoryEditorKey({owner:getStoredUser()?.id ?? 'legacy',session:clientSession().snapshot().token,epoch:clientSession().snapshot().epoch});
  return <RepositoriesEditor key={key} />;
}

function RepositoriesEditor() {
  const t = useText();
  const fieldId = useId();
  const navigate = useNavigate();
  const location = useLocation();
  const removalNotice = readBranchPairRemovalNotice(location.state) ?? readPersistedBranchPairRemovalNotice();
  const managedRemovalReceiptNotices = readAllManagedRemovalReceipts().filter(
    shouldDisplayManagedRemovalReceipt,
  );
  const queryClient = useQueryClient();
  const user = getStoredUser();
  const isAdmin = user?.role === 'admin';

  const [showAddModal, setShowAddModal] = useState(false);
  const [form, setForm] = useState<AddRepoForm>({ ...defaultForm });
  const [svnTestResult, setSvnTestResult] = useState<ConnectionTestOutcome | null>(null);
  const [gitTestResult, setGitTestResult] = useState<ConnectionTestOutcome | null>(null);
  const [svnTesting, setSvnTesting] = useState(false);
  const [gitTesting, setGitTesting] = useState(false);
  const saveFlow=useRepositorySave(undefined,showAddModal);
  const [saveMessage,setSaveMessage]=useState<string | null>(null);
  const [saveError,setSaveError]=useState<string | null>(null);
  const currentForm=useRef(form);currentForm.current=form;
  const dialogVersion=useRef(0);
  useEffect(()=>{
    if(showAddModal && saveFlow.initial) {
      const previous=saveFlow.initial;
      setForm({...defaultForm,...(previous.draftConfiguration ?? previous.configuration),svn_password:'',git_token:'',
        initial_import_mode:((previous.draftPreferences ?? previous.preferences)?.importMode ?? DEFAULT_REPO_IMPORT_MODE) as RepoImportMode,
        initial_svn_revision:(previous.draftPreferences ?? previous.preferences)?.svnRevision ?? 'HEAD'});
    }
  },[showAddModal,saveFlow.initial]);
  useEffect(()=>{
    if(saveFlow.attempt?.repoId) void queryClient.invalidateQueries({queryKey:['repos']});
  },[saveFlow.attempt?.repoId,queryClient]);


  const { data: repos, isLoading, isError, error } = useQuery({
    queryKey: ['repos'],
    queryFn: api.getRepos,
  });

  const repoList=repos ?? [];
  const parents=repoList.filter(repo=>!repo.parent_id);

  function setField<K extends keyof AddRepoForm>(key: K, value: AddRepoForm[K]) {
    const next={...currentForm.current,[key]:value};currentForm.current=next;setForm(next);
    saveFlow.rememberDraft(next,{importMode:next.initial_import_mode,svnRevision:next.initial_svn_revision});
  }
  async function loadCurrentSettingsForReview() {
    const generation=dialogVersion.current;
    try {
      const current=await saveFlow.loadCurrentForReview();
      if(!saveFlow.viewActive() || generation!==dialogVersion.current)return;
      setForm(previous=>({...previous,...publicConfiguration(current),svn_password:'',git_token:''}));setSaveError(null);setSaveMessage('Review these current settings on the existing ID. Required credentials were cleared.');
    } catch(cause) {if(saveFlow.viewActive() && generation===dialogVersion.current)setSaveError(cause instanceof Error ? cause.message : 'Current settings could not be read.');}
  }
  function useReviewedSettings() {
    const live=currentForm.current;
    try {saveFlow.useReviewedSettings(live,{svn_password:live.svn_password || undefined,git_token:live.git_token || undefined},{importMode:live.initial_import_mode,svnRevision:live.initial_svn_revision});setSaveError(null);setSaveMessage('New save prepared on the existing repository; earlier outcomes are retained.');}
    catch(cause) {setSaveError(cause instanceof Error ? cause.message : 'Reviewed recovery could not be prepared.');}
  }
  function openAdd() {dialogVersion.current++;setSaveMessage(null);setSaveError(null);setShowAddModal(true);}
  function closeAdd() {
    saveFlow.rememberDraft(currentForm.current,{importMode:currentForm.current.initial_import_mode,svnRevision:currentForm.current.initial_svn_revision});dialogVersion.current++;setShowAddModal(false);
    setForm({...defaultForm});setSaveError(null);setSaveMessage(null);
  }
  async function handleCreate() {
    const generation=dialogVersion.current;
    const submitted={...currentForm.current};setSaveError(null);setSaveMessage(null);
    try {
      if(canContinueWithPreferences(saveFlow.attempt,submitted,{svn_password:submitted.svn_password,git_token:submitted.git_token})) {
        const previous=saveFlow.attempt;saveFlow.finish(previous.configRequestId);setShowAddModal(false);setForm({...defaultForm});
        navigate(`/repos/${previous.repoId}`,{state:{importPrefs:{importMode:submitted.initial_import_mode,svnRevision:submitted.initial_svn_revision}}});return;
      }
      const result=await saveFlow.save(submitted,{svn_password:submitted.svn_password || undefined,git_token:submitted.git_token || undefined},undefined,
        {importMode:submitted.initial_import_mode,svnRevision:submitted.initial_svn_revision});
      if(!result.viewActive || dialogVersion.current!==generation)return;
      void queryClient.invalidateQueries({queryKey:['repos']});
      const live=currentForm.current;
      const same= samePublicConfiguration(live,result.configuration)
        && (live.svn_password || '')===(result.credentials.svn_password || '')
        && (live.git_token || '')===(result.credentials.git_token || '')
        && sameImportPreferences({importMode:live.initial_import_mode,svnRevision:live.initial_svn_revision},result.preferences);
      if(!same) {setForm(previous=>({...previous,...retainUnsavedCredentials(previous,result.credentials)}));setSaveMessage('The earlier save completed on the existing ID. Newer entered changes remain unsaved; save again to update that registration.');return;}
      saveFlow.finish(result.attempt.configRequestId);
      setShowAddModal(false);setForm({...defaultForm});setSvnTestResult(null);setGitTestResult(null);
      navigate(`/repos/${result.attempt.repoId}`,{state:{importPrefs:{importMode:submitted.initial_import_mode,svnRevision:submitted.initial_svn_revision}}});
    } catch(cause) {
      if(saveFlow.viewActive() && dialogVersion.current===generation)setSaveError(cause instanceof Error ? cause.message : 'Save is not confirmed; the recovery record was retained.');
    }
  }

  async function handleTestSvn() {
    setSvnTesting(true);
    setSvnTestResult(null);
    try {
      const result = await api.testSvnConnection({
        url: form.svn_url + (form.svn_branch ? '/' + form.svn_branch.replace(/^\/+/, '') : ''),
        username: form.svn_username,
        password: form.svn_password || undefined,
      });
      setSvnTestResult({ ok: result.ok === true, message: result.message, testedAt: new Date() });
    } catch (e: unknown) {
      setSvnTestResult({ ok: false, message: e instanceof Error ? e.message : String(e), testedAt: new Date() });
    } finally {
      setSvnTesting(false);
    }
  }

  async function handleTestGit() {
    setGitTesting(true);
    setGitTestResult(null);
    try {
      const result = await api.testGitConnection({
        api_url: form.git_api_url,
        repo: form.git_repo,
        provider: form.git_provider,
        token: form.git_token || undefined,
      });
      setGitTestResult({ ok: result.ok === true, message: result.message, testedAt: new Date() });
    } catch (e: unknown) {
      setGitTestResult({ ok: false, message: e instanceof Error ? e.message : String(e), testedAt: new Date() });
    } finally {
      setGitTesting(false);
    }
  }

  if (isLoading) {
    return (
      <div className="space-y-6">
        {removalNotice && <BranchPairRemovalNotice notice={removalNotice} />}
        {managedRemovalReceiptNotices.map((receipt) => (
          <ManagedRemovalReceiptNotice key={`${receipt.repoId}:${receipt.operationId}`} receipt={receipt} />
        ))}
        <div className="text-center py-8 text-gray-400">Loading repositories...</div>
      </div>
    );
  }

  if (isError) {
    return (
      <div className="space-y-6">
        {removalNotice && <BranchPairRemovalNotice notice={removalNotice} />}
        {managedRemovalReceiptNotices.map((receipt) => (
          <ManagedRemovalReceiptNotice key={`${receipt.repoId}:${receipt.operationId}`} receipt={receipt} />
        ))}
        <div className="text-center py-8 text-red-400">
          Error loading repositories: {error?.message ?? 'Unknown error'}
        </div>
      </div>
    );
  }

  const childrenByParent = new Map<string, Repository[]>();
  for (const repo of repoList) {
    if (repo.parent_id) {
      const list = childrenByParent.get(repo.parent_id) ?? [];
      list.push(repo);
      childrenByParent.set(repo.parent_id, list);
    }
  }

  return (
    <div className="space-y-6">
      {removalNotice && <BranchPairRemovalNotice notice={removalNotice} />}
      {managedRemovalReceiptNotices.map((receipt) => (
        <ManagedRemovalReceiptNotice key={`${receipt.repoId}:${receipt.operationId}`} receipt={receipt} />
      ))}
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="rs-title text-2xl font-bold text-gray-100">Repositories</h1>
          <span className="rs-mark inline-flex items-center justify-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-blue-900/50 text-blue-300">
            {parents.length}
          </span>
        </div>
        {isAdmin && (
          <button
            onClick={openAdd}
            className="rs-btn rs-btn--primary inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 rs-text-on-accent text-sm font-medium transition-colors"
          >
            <Plus className="w-4 h-4" />
            {t('repos.add')}
          </button>
        )}
      </div>

      {/* Repository List */}
      {parents.length === 0 ? (
        /* Empty State */
        <div className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg py-20 px-8 text-center flex flex-col items-center">
          <SyncArrowsIcon className="w-16 h-16 mb-6 opacity-60" />
          <p className="text-gray-300 text-xl font-semibold">No repositories configured</p>
          <p className="text-gray-500 text-sm mt-2 mb-6">
            Add your first SVN &#x2194; Git sync
          </p>
          {isAdmin && (
            <button
              onClick={openAdd}
              className="rs-btn rs-btn--primary inline-flex items-center gap-2 px-5 py-2.5 rounded-lg bg-blue-600 hover:bg-blue-700 rs-text-on-accent text-sm font-medium transition-colors"
            >
              <Plus className="w-4 h-4" />
              {t('repos.add')}
            </button>
          )}
        </div>
      ) : (
        <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4">
          {parents.map((repo: Repository) => {
            return (
              <div key={repo.id} className="rs-sec group/card bg-gray-800/60 border border-gray-700 rounded-lg overflow-hidden hover:border-blue-500/50 transition-colors">
                {/* Parent Repo Card */}
                <button
                  onClick={() => navigate(`/repos/${repo.id}`)}
                  className="w-full p-5 text-left"
                >
                  <div className="flex items-start gap-4">
                    {/* Sync Icon */}
                    <div className="flex-shrink-0 mt-0.5">
                      <SyncArrowsIcon className="w-9 h-9" />
                    </div>

                    {/* Main Content */}
                    <div className="flex-1 min-w-0">
                      {/* Name + Badge row */}
                      <div className="flex items-center gap-3 mb-2.5">
                        <h3 className="rs-heading text-lg font-semibold text-gray-100 truncate group-hover/card:text-blue-400 transition-colors">
                          {repo.name}
                        </h3>
                        <span
                          className={`rs-mark inline-flex items-center px-2 py-0.5 rounded text-xs font-medium flex-shrink-0 ${
                            repo.enabled
                              ? 'bg-blue-900/50 text-blue-300'
                              : 'bg-gray-700 text-gray-400'
                          }`}
                        >
                          {repo.sync_status === 'archived' ? 'Archived' : repo.enabled ? 'Configured enabled' : 'Configured disabled'}
                        </span>
                      </div>

                      {/* SVN URL + Git repo */}
                      <div className="space-y-1.5 mb-3">
                        <div className="flex items-center gap-2 text-sm text-gray-400">
                          <Database className="w-3.5 h-3.5 flex-shrink-0 text-blue-400" />
                          <span className="truncate">{repo.svn_url}</span>
                        </div>
                        <div className="flex items-center gap-2 text-sm text-gray-400">
                          <GitBranch className="w-3.5 h-3.5 flex-shrink-0 text-purple-400" />
                          <span className="truncate">{repo.git_repo}</span>
                        </div>
                      </div>

                    </div>
                  </div>
                </button>
                <div className="px-5 pb-4"><RepositoryStatusSummary repoId={repo.id}/></div>

                {/* Branch Pairs inside parent card (recursive) */}
                <BranchTree repoId={repo.id} childMap={childrenByParent} navigate={navigate} depth={0} />
              </div>
            );
          })}
        </div>
      )}

      {/* Add Repository Modal */}
      {showAddModal && (
        <Dialog title={t('repos.add')} description="Configure the registered SVN and Git endpoints and import policy. Review saved and pending stages before retrying." onClose={closeAdd} busy={saveFlow.pending}>
          <div className="rs-dlg bg-gray-800 border border-gray-700 rounded-lg shadow-xl w-full max-w-2xl max-h-[90vh] overflow-y-auto">
            {/* Modal header */}
            <div className="flex items-center justify-between p-6 border-b border-gray-700">

              <button
                onClick={closeAdd}
                aria-label="Close dialog" data-dialog-cancel disabled={saveFlow.pending}
                className="text-gray-400 hover:text-gray-200 transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <div className="p-6 space-y-6">
              <RepositorySaveNotice attempt={saveFlow.attempt} error={saveError ?? saveFlow.error} pending={saveFlow.pending} onLoadCurrent={loadCurrentSettingsForReview} onUseReviewed={useReviewedSettings} reviewed={!!saveFlow.reviewedCurrent} />
              {saveMessage && <p role="status" className="text-yellow-300">{saveMessage}</p>}

              {/* Name */}
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-1" htmlFor={`${fieldId}-1`}>{t('repos.add.name')}</label>
                <input
                  type="text"
                  className={inputClass}
                  value={form.name}
                  onChange={(e) => setField('name', e.target.value)}
                  placeholder="My Project" id={`${fieldId}-1`}
                />
              </div>

              {/* SVN Section */}
              <div>
                <h3 className="rs-heading text-sm font-medium text-gray-300 mb-3 flex items-center gap-2">
                  <Database className="w-4 h-4 text-blue-400" />
                  {t('repos.add.svn')}
                </h3>
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-2`}>{t('field.svnUrl')}</label>
                    <input
                      type="text"
                      className={inputClass}
                      value={form.svn_url}
                      onChange={(e) => setField('svn_url', e.target.value)}
                      placeholder="https://svn.example.com/repo" id={`${fieldId}-2`}
                    />
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-3`}>Branch</label>
                    <input
                      type="text"
                      className={inputClass}
                      value={form.svn_branch}
                      onChange={(e) => setField('svn_branch', e.target.value)}
                      placeholder="trunk" id={`${fieldId}-3`}
                    />
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-4`}>Username</label>
                    <input
                      type="text"
                      className={inputClass}
                      value={form.svn_username}
                      onChange={(e) => setField('svn_username', e.target.value)}
                      placeholder="svn-user" id={`${fieldId}-4`}
                    />
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-5`}>{t('field.svnPassword')}</label>
                    <input
                      type="password"
                      className={inputClass}
                      value={form.svn_password}
                      onChange={(e) => setField('svn_password', e.target.value)}
                      placeholder="Enter SVN password" id={`${fieldId}-5`}
                    />
                  </div>
                  <div className="md:col-span-2">
                    <button
                      type="button"
                      onClick={handleTestSvn}
                      disabled={svnTesting || !form.svn_url}
                      className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-blue-600 text-blue-300 hover:bg-blue-900/30 disabled:opacity-50 transition-colors"
                    >
                      {svnTesting ? 'Testing...' : 'Test SVN Connection'}
                    </button>
                    <ConnectionTestResult result={svnTestResult} />
                  </div>
                </div>
              </div>

              {/* Git Section */}
              <div>
                <h3 className="rs-heading text-sm font-medium text-gray-300 mb-3 flex items-center gap-2">
                  <GitBranch className="w-4 h-4 text-purple-400" />
                  {t('repos.add.git')}
                </h3>
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-6`}>Provider</label>
                    <select
                      className={selectClass}
                      value={form.git_provider}
                      onChange={(e) => setField('git_provider', e.target.value)} id={`${fieldId}-6`}
                    >
                      <option value="github">GitHub</option>
                      <option value="gitea">Gitea</option>
                    </select>
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-7`}>API URL</label>
                    <input
                      type="text"
                      className={inputClass}
                      value={form.git_api_url}
                      onChange={(e) => setField('git_api_url', e.target.value)}
                      placeholder="https://api.github.com" id={`${fieldId}-7`}
                    />
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-8`}>Repository</label>
                    <input
                      type="text"
                      className={inputClass}
                      value={form.git_repo}
                      onChange={(e) => setField('git_repo', e.target.value)}
                      placeholder="owner/repo" id={`${fieldId}-8`}
                    />
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-9`}>{t('field.defaultBranch')}</label>
                    <input
                      type="text"
                      className={inputClass}
                      value={form.git_branch}
                      onChange={(e) => setField('git_branch', e.target.value)}
                      placeholder="main" id={`${fieldId}-9`}
                    />
                  </div>
                  <div className="md:col-span-2">
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-10`}>{t('field.gitToken')}</label>
                    <input
                      type="password"
                      className={inputClass}
                      value={form.git_token}
                      onChange={(e) => setField('git_token', e.target.value)}
                      placeholder="Enter Git API token" id={`${fieldId}-10`}
                    />
                  </div>
                  <div className="md:col-span-2">
                    <button
                      type="button"
                      onClick={handleTestGit}
                      disabled={gitTesting || !form.git_repo}
                      className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-purple-600 text-purple-300 hover:bg-purple-900/30 disabled:opacity-50 transition-colors"
                    >
                      {gitTesting ? 'Testing...' : 'Test Git Connection'}
                    </button>
                    <ConnectionTestResult result={gitTestResult} />
                  </div>
                </div>
              </div>

              {/* Initial import baseline */}
              <div>
                <h3 className="rs-heading text-sm font-medium text-gray-300 mb-3">Initial SVN import</h3>
                <ImportModeFields
                  mode={form.initial_import_mode}
                  svnRevision={form.initial_svn_revision}
                  onModeChange={(mode) => setField('initial_import_mode', mode)}
                  onRevisionChange={(value) => setField('initial_svn_revision', value)}
                />
              </div>

              {/* Sync Section */}
              <div>
                <h3 className="rs-heading text-sm font-medium text-gray-300 mb-3 flex items-center gap-2">
                  <Clock className="w-4 h-4 text-green-400" />
                  {t('repos.add.sync')}
                </h3>
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-11`}>{t('field.syncMode')}</label>
                    <select
                      className={selectClass}
                      value={form.sync_mode}
                      onChange={(e) => setField('sync_mode', e.target.value)} id={`${fieldId}-11`}
                    >
                      <option value="direct">Direct</option>
                      <option value="pr">Pull Request</option>
                    </select>
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-12`}>{t('repos.add.pollInterval')}</label>
                    <input
                      type="number"
                      className={inputClass}
                      value={form.poll_interval_secs}
                      onChange={(e) => setField('poll_interval_secs', Number(e.target.value))}
                      min={10} id={`${fieldId}-12`}
                    />
                  </div>
                  <div>
                    <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-13`}>{t('repos.add.lfsThreshold')}</label>
                    <input
                      type="number"
                      className={inputClass}
                      value={form.lfs_threshold_mb}
                      onChange={(e) => setField('lfs_threshold_mb', Number(e.target.value))}
                      min={0} id={`${fieldId}-13`}
                    />
                  </div>
                  <div className="flex items-end gap-3">
                    <div>
                      <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-14`}>{t('field.autoMerge')}</label>
                      <button
                        type="button"
                        role="switch"
                        aria-checked={form.auto_merge}
                        aria-label="Auto merge"
                        onClick={() => setField('auto_merge', !form.auto_merge)}
                        className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                          form.auto_merge ? 'bg-blue-600' : 'bg-gray-600'
                        }`} id={`${fieldId}-14`}
                      >
                        <span
                          className={`inline-block h-4 w-4 transform rounded-full rs-knob transition-transform ${
                            form.auto_merge ? 'translate-x-6' : 'translate-x-1'
                          }`}
                        />
                      </button>
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-15`}>Enabled</label>
                      <button
                        type="button"
                        role="switch"
                        aria-checked={form.enabled}
                        aria-label="Enabled"
                        onClick={() => setField('enabled', !form.enabled)}
                        className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                          form.enabled ? 'bg-green-600' : 'bg-gray-600'
                        }`} id={`${fieldId}-15`}
                      >
                        <span
                          className={`inline-block h-4 w-4 transform rounded-full rs-knob transition-transform ${
                            form.enabled ? 'translate-x-6' : 'translate-x-1'
                          }`}
                        />
                      </button>
                    </div>
                  </div>
                </div>
              </div>
            </div>

            {/* Modal footer */}
            <div className="flex items-center justify-end gap-3 p-6 border-t border-gray-700">
              <button
                onClick={closeAdd}
                className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
               data-dialog-cancel disabled={saveFlow.pending}>
                Cancel
              </button>
              <button
                onClick={handleCreate}
                disabled={saveFlow.pending || !form.name.trim()}
                className="rs-btn rs-btn--primary inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 disabled:opacity-50 rs-text-on-accent text-sm font-medium transition-colors"
              >
                <Plus className="w-4 h-4" />
                {saveFlow.pending ? 'Saving...' : saveFlow.attempt?.repoId ? 'Resume / Save on Existing ID' : saveFlow.attempt ? 'Resume Same Save Request' : 'Create Repository'}
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </div>
  );
}

/** Recursive component that renders branch pairs and their children at any depth. */
function BranchTree({
  repoId,
  childMap,
  navigate,
  depth,
}: {
  repoId: string;
  childMap: Map<string, Repository[]>;
  navigate: (path: string) => void;
  depth: number;
}) {
  const children = childMap.get(repoId) ?? [];
  if (children.length === 0) return null;

  return (
    <div
      className={`border-t border-gray-700/50 bg-gray-900/30 px-5 py-2 space-y-0.5 ${depth > 0 ? 'border-t-0 pl-8' : ''}`}
    >
      {depth === 0 && (
        <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wider text-gray-500 mb-1 pl-1">
          <GitBranch className="w-3 h-3" />
          <span>Branches</span>
        </div>
      )}
      {children.map((child) => (
        <div key={child.id}>
          <button
            onClick={() => navigate(`/repos/${child.id}`)}
            className="w-full flex items-center gap-2 px-2 py-1.5 rounded hover:bg-gray-700/40 transition-colors text-left group/branch"
          >
            <GitBranch className="w-3 h-3 text-purple-400 flex-shrink-0" />
            <span className="text-xs font-medium text-gray-300 group-hover/branch:text-purple-300 transition-colors truncate">
              {child.name.split(' / ').pop()}
            </span>
            <span className="text-[10px] text-blue-400/60">{child.svn_branch}</span>
            <ArrowRight className="w-2.5 h-2.5 text-gray-600 flex-shrink-0" />
            <span className="text-[10px] text-purple-400/60">{child.git_branch}</span>
            <span className="ml-auto flex items-center gap-1.5 flex-shrink-0">
              <span className="text-[10px] text-gray-400">{child.sync_status === 'archived' ? 'Archived · sync stopped' : `Configuration: ${child.enabled ? 'enabled' : 'disabled'}`}</span>
            </span>
          </button>
          <RepositoryStatusSummary repoId={child.id}/>
          {/* Recurse into this child's children */}
          <BranchTree repoId={child.id} childMap={childMap} navigate={navigate} depth={depth + 1} />
        </div>
      ))}
    </div>
  );
}
