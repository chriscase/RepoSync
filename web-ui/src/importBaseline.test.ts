import { describe, expect, it } from 'vitest';
import {
  buildStartImportBody,
  importBaselineFailureNotice,
  importHistoryNotice,
  parseSvnRevisionInput,
} from './importBaseline';

describe('importBaseline', () => {
  it('defaults full import to an empty POST body', () => {
    expect(buildStartImportBody('full', 'HEAD')).toEqual({});
  });

  it('builds snapshot import requests with parsed positive integer', () => {
    expect(buildStartImportBody('snapshot', '  4  ')).toEqual({
      import_mode: 'snapshot',
      svn_revision: '4',
    });
  });

  it('shows boundary copy only when the server recorded a verified pin', () => {
    const notice = importHistoryNotice({
      import_mode: 'snapshot',
      starting_revision: 3,
      history_boundary: 'SVN history before r3 was not imported; later revisions remain pending',
      snapshot_pin: {
        svn_uuid: 'u',
        canonical_url: 'file:///t',
        operative_rev: 3,
        peg_rev: 3,
        requested: '3',
      },
    });
    expect(notice).toContain('before r3');
    expect(notice?.toLowerCase()).not.toContain('baseline forward');
  });

  it('does not invent a baseline for refused snapshot import', () => {
    expect(
      importHistoryNotice({
        import_mode: 'snapshot',
        lifecycle: 'failed',
        earlier_history_imported: true,
      }),
    ).toBeNull();
    const failure = importBaselineFailureNotice({
      import_mode: 'snapshot',
      lifecycle: 'failed',
      outcome_detail: 'invalid or inaccessible snapshot revision',
    });
    expect(failure).toContain('revision');
    expect(failure?.toLowerCase()).not.toContain('verified snapshot baseline');
  });

  it('rejects non–base-10 svn revision input', () => {
    expect(() => parseSvnRevisionInput('0')).toThrow();
    expect(() => parseSvnRevisionInput('tip')).toThrow();
    expect(() => parseSvnRevisionInput('2.0')).toThrow();
    expect(() => parseSvnRevisionInput('1e2')).toThrow();
    expect(() => parseSvnRevisionInput('0x10')).toThrow();
    expect(() => parseSvnRevisionInput('2abc')).toThrow();
    expect(parseSvnRevisionInput('HEAD')).toBe('HEAD');
    expect(parseSvnRevisionInput('  42  ')).toBe('42');
  });
});
