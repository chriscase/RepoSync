import { describe, expect, it } from 'vitest';
import {
  buildStartImportBody,
  importHistoryNotice,
  parseSvnRevisionInput,
} from './importBaseline';

describe('importBaseline', () => {
  it('defaults full import to an empty POST body', () => {
    expect(buildStartImportBody('full', 'HEAD')).toEqual({});
  });

  it('builds snapshot import requests with explicit revision', () => {
    expect(buildStartImportBody('snapshot', '  4  ')).toEqual({
      import_mode: 'snapshot',
      svn_revision: '4',
    });
  });

  it('never implies earlier history for snapshot status', () => {
    const notice = importHistoryNotice({
      import_mode: 'snapshot',
      starting_revision: 3,
      earlier_history_imported: false,
    });
    expect(notice).toContain('before r3');
    expect(notice?.toLowerCase()).not.toContain('full history');
  });

  it('rejects invalid svn revision input', () => {
    expect(() => parseSvnRevisionInput('0')).toThrow();
    expect(() => parseSvnRevisionInput('tip')).toThrow();
    expect(parseSvnRevisionInput('HEAD')).toBe('HEAD');
  });
});
