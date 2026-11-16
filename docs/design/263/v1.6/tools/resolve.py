"""Resolve matrix anchors to line numbers at both epic revisions (read-only git show)."""
import subprocess, sys, json, functools, os
sys.path.insert(0, 'gen')
import matrix_data as M
REPO = os.environ.get('REPOSYNC_REPO') or os.getcwd()   # a clone of the repository; read with git show only
P = 'b2b055a72657824fd6662b451f69718d73405aa5'   # epic/reposync-partial-52
R = '5853a8884496d294ce21898415d7d2718b73bf7c'   # epic/reposync-reliability-61

def path(f):
    return f if f.startswith(('crates/', 'docs/', 'web-ui/')) else 'web-ui/src/' + f

@functools.lru_cache(None)
def show(rev, p):
    r = subprocess.run(['git', '-C', REPO, 'show', f'{rev}:{p}'], capture_output=True, text=True)
    return r.stdout.split('\n') if r.returncode == 0 else None

def find(rev, p, anchor):
    lines = show(rev, p)
    if lines is None:
        return None, 'file missing'
    hits = [i + 1 for i, l in enumerate(lines) if anchor in l]
    if not hits:
        return None, 'anchor missing'
    return hits[0], len(hits)

def resolve():
    out, bad = {}, []
    for r in M.ROWS:
        p = path(r['file'])
        lp, np_ = find(P, p, r['anchor'])
        lr, nr = find(R, p, r['anchor_r'])
        out[r['id']] = dict(path=p, p=lp, r=lr, np=np_, nr=nr)
        if lp is None or lr is None:
            bad.append((r['id'], p, r['anchor'], np_, r['anchor_r'], nr))
    return out, bad

if __name__ == '__main__':
    out, bad = resolve()
    print(len(M.ROWS), 'rows;', len(bad), 'unresolved')
    for b in bad: print('  ', b)
    ids = [r['id'] for r in M.ROWS]
    print('duplicate ids:', [i for i in set(ids) if ids.count(i) > 1])
    json.dump(out, open('gen/anchors.json', 'w'), indent=1)
