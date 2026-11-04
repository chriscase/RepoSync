import { useId } from 'react';
import { LoadingSpinner } from '../components/LoadingSpinner';
import { useState, useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, type SaveLdapConfigRequest } from '../api';

export default function AdminLdap() {
  const fieldId = useId();
  const queryClient = useQueryClient();

  // Check admin role
  const currentUser = (() => {
    try {
      const stored = localStorage.getItem('user');
      return stored ? JSON.parse(stored) : null;
    } catch {
      return null;
    }
  })();

  const { data: ldapConfig, isLoading } = useQuery({
    queryKey: ['ldap-config'],
    queryFn: api.getLdapConfig,
    retry: false,
  });

  const [enabled, setEnabled] = useState(false);
  const [url, setUrl] = useState('');
  const [baseDn, setBaseDn] = useState('');
  const [searchFilter, setSearchFilter] = useState('(&(objectClass=user)(name={0}))');
  const [displayNameAttr, setDisplayNameAttr] = useState('displayname');
  const [emailAttr, setEmailAttr] = useState('mail');
  const [groupAttr, setGroupAttr] = useState('memberOf');
  const [bindDn, setBindDn] = useState('');
  const [bindPassword, setBindPassword] = useState('');
  const [bindPasswordSet, setBindPasswordSet] = useState(false);

  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [message, setMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

  // Populate form from loaded config
  useEffect(() => {
    if (ldapConfig) {
      setEnabled(ldapConfig.enabled);
      setUrl(ldapConfig.url);
      setBaseDn(ldapConfig.base_dn);
      setSearchFilter(ldapConfig.search_filter);
      setDisplayNameAttr(ldapConfig.display_name_attr);
      setEmailAttr(ldapConfig.email_attr);
      setGroupAttr(ldapConfig.group_attr);
      setBindDn(ldapConfig.bind_dn);
      setBindPasswordSet(ldapConfig.bind_password_set);
    }
  }, [ldapConfig]);

  if (currentUser && currentUser.role !== 'admin') {
    return (
      <div className="text-center py-12">
        <h2 className="text-xl font-semibold text-gray-100 mb-2">Access Denied</h2>
        <p className="text-gray-400">You need admin privileges to view this page.</p>
      </div>
    );
  }

  if (isLoading) {
    return <LoadingSpinner message="Loading LDAP configuration..." />;
  }

  const buildRequest = (): SaveLdapConfigRequest => ({
    enabled,
    url,
    base_dn: baseDn,
    search_filter: searchFilter,
    display_name_attr: displayNameAttr,
    email_attr: emailAttr,
    group_attr: groupAttr,
    bind_dn: bindDn || undefined,
    bind_password: bindPassword || undefined,
  });

  const handleSave = async () => {
    setSaving(true);
    setMessage(null);
    try {
      const result = await api.saveLdapConfig(buildRequest());
      setMessage({ type: 'success', text: result.message || 'LDAP configuration saved' });
      setBindPassword('');
      queryClient.invalidateQueries({ queryKey: ['ldap-config'] });
    } catch (err) {
      setMessage({
        type: 'error',
        text: err instanceof Error ? err.message : 'Failed to save configuration',
      });
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    setTesting(true);
    setMessage(null);
    try {
      const result = await api.testLdapConnection(buildRequest());
      setMessage({
        type: result.ok ? 'success' : 'error',
        text: result.message,
      });
    } catch (err) {
      setMessage({
        type: 'error',
        text: err instanceof Error ? err.message : 'Connection test failed',
      });
    } finally {
      setTesting(false);
    }
  };

  const inputClass =
    'w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-500 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm';

  return (
    <div className="max-w-3xl mx-auto space-y-6">
      <div>
        <h1 className="text-2xl font-bold text-gray-100">LDAP Configuration</h1>
        <p className="mt-1 text-gray-400">
          Configure LDAP/Active Directory authentication. When enabled, users can log in with
          their corporate credentials. New accounts are auto-provisioned on first login.
        </p>
      </div>

      {message && (
        <div
          className={`px-4 py-3 rounded text-sm ${
            message.type === 'success'
              ? 'bg-green-900/40 border border-green-700 text-green-300'
              : 'bg-red-900/40 border border-red-700 text-red-300'
          }`}
        >
          {message.text}
        </div>
      )}

      <div className="bg-gray-800 rounded-lg border border-gray-700 p-6 space-y-6">
        {/* Enable/Disable */}
        <div className="flex items-center gap-3">
          <input
            type="checkbox"
            id="ldap-enabled"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
            className="h-4 w-4 rounded border-gray-600 bg-gray-700 text-blue-600 focus:ring-blue-500"
          />
          <label htmlFor="ldap-enabled" className="text-sm font-medium text-gray-200">
            Enable LDAP Authentication
          </label>
        </div>

        {/* Server URL */}
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-1`}>Server URL</label>
          <input
            type="text"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            className={inputClass}
            placeholder="ldaps://ldap.example.com:3269" id={`${fieldId}-1`}
          />
          <p className="mt-1 text-xs text-gray-300">
            Use ldaps:// for TLS connections or ldap:// for plain connections.
          </p>
        </div>

        {/* Base DN */}
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-2`}>Base DN</label>
          <input
            type="text"
            value={baseDn}
            onChange={(e) => setBaseDn(e.target.value)}
            className={inputClass}
            placeholder="dc=corp,dc=example,dc=com" id={`${fieldId}-2`}
          />
        </div>

        {/* Search Filter */}
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-3`}>User Search Filter</label>
          <input
            type="text"
            value={searchFilter}
            onChange={(e) => setSearchFilter(e.target.value)}
            className={inputClass}
            placeholder="(&(objectClass=user)(name={0}))" id={`${fieldId}-3`}
          />
          <p className="mt-1 text-xs text-gray-300">
            Use {'{0}'} as a placeholder for the username.
          </p>
        </div>

        {/* Attribute Mappings */}
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <div>
            <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-4`}>Display Name Attribute</label>
            <input
              type="text"
              value={displayNameAttr}
              onChange={(e) => setDisplayNameAttr(e.target.value)}
              className={inputClass}
              placeholder="displayname" id={`${fieldId}-4`}
            />
          </div>
          <div>
            <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-5`}>Email Attribute</label>
            <input
              type="text"
              value={emailAttr}
              onChange={(e) => setEmailAttr(e.target.value)}
              className={inputClass}
              placeholder="mail" id={`${fieldId}-5`}
            />
          </div>
          <div>
            <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-6`}>Group Membership Attribute</label>
            <input
              type="text"
              value={groupAttr}
              onChange={(e) => setGroupAttr(e.target.value)}
              className={inputClass}
              placeholder="memberOf" id={`${fieldId}-6`}
            />
          </div>
        </div>

        {/* Service Account */}
        <div className="border-t border-gray-700 pt-6">
          <h3 className="text-sm font-semibold text-gray-300 mb-3">
            Service Account (Optional)
          </h3>
          <p className="text-xs text-gray-300 mb-4">
            A service account is used to search for user DNs before binding. If not provided,
            a direct bind with the constructed DN will be attempted.
          </p>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-7`}>Bind DN</label>
              <input
                type="text"
                value={bindDn}
                onChange={(e) => setBindDn(e.target.value)}
                className={inputClass}
                placeholder="cn=svc-reposync,ou=service,dc=corp,dc=example,dc=com" id={`${fieldId}-7`}
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-8`}>
                Bind Password
                {bindPasswordSet && !bindPassword && (
                  <span className="ml-2 text-xs text-green-400">(saved)</span>
                )}
              </label>
              <input
                type="password"
                value={bindPassword}
                onChange={(e) => setBindPassword(e.target.value)}
                className={inputClass}
                placeholder={bindPasswordSet ? 'Leave blank to keep current' : 'Service account password'} id={`${fieldId}-8`}
              />
            </div>
          </div>
        </div>

        {/* Actions */}
        <div className="flex items-center gap-3 pt-4 border-t border-gray-700">
          <button
            onClick={handleSave}
            disabled={saving}
            className="px-4 py-2 text-sm font-medium rounded-md text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
          >
            {saving ? 'Saving...' : 'Save Configuration'}
          </button>
          <button
            onClick={handleTest}
            disabled={testing || !url}
            className="px-4 py-2 text-sm font-medium rounded-md text-gray-200 bg-gray-700 hover:bg-gray-600 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-gray-500"
          >
            {testing ? 'Testing...' : 'Test Connection'}
          </button>
        </div>
      </div>
    </div>
  );
}
