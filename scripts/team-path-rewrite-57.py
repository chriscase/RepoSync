"""Disposable Team #57 scheduler qualification. Requires svn, svnadmin, svnlook, git."""
import contextlib
import json
import os
import pathlib
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
daemon = str(pathlib.Path(sys.argv[1]).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix="rs57-pair-"))
print(root, flush=True)
repo=root/'svn'; bare=root/'git'/'local'/'fixture.git'; bare.parent.mkdir(parents=True)
base=root/'git'; data=root/'service'; db=data/'reposync.db'; wc=root/'svnwc'; clone=root/'gitwc'; mirror=root/'mirror'; cfg=root/'fixture.toml'
home=root/'home'; home.mkdir()
clean_env = {k: v for k, v in os.environ.items() if not k.startswith(('REPOSYNC_', 'GIT_'))}
env=dict(clean_env, HOME=str(home), GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1', GIT_TERMINAL_PROMPT='0', GIT_AUTHOR_NAME='Fixture Developer', GIT_AUTHOR_EMAIL='fixture@example.invalid', GIT_COMMITTER_NAME='Fixture Developer', GIT_COMMITTER_EMAIL='fixture@example.invalid', REPOSYNC_ADMIN_PASSWORD='fixture-only-password-932', REPOSYNC_SESSION_SECRET='fixture-only-session-long-enough-932')
def cmd(*a):
 return subprocess.check_output([str(x) for x in a],env=env,text=True,stderr=subprocess.STDOUT,timeout=120).strip()
def wait(f,name,limit=75):
 deadline=time.monotonic()+limit; err=None
 while time.monotonic()<deadline:
  try:
   r=f()
   if r: return r
  except (subprocess.CalledProcessError, OSError, ValueError) as exc: err=exc
  time.sleep(.35)
 raise RuntimeError(f'{name} timed out: {err}')
def rows(sql):
 with contextlib.closing(sqlite3.connect(f'{db.as_uri()}?mode=ro',uri=True)) as q: return q.execute(sql).fetchall()
def sha(): return cmd('git','--git-dir',bare,'rev-parse','refs/heads/main')
def rev(): return int(cmd('svnlook','youngest',repo))
def svncat(p): return cmd('svn','cat',f'{repo.as_uri()}/trunk/{p}')
def gcat(p): return cmd('git','--git-dir',bare,'show',f'main:{p}')
cmd('svnadmin','create',repo); cmd('svn','mkdir',f'{repo.as_uri()}/trunk','-m','bootstrap r1')
cmd('svn','checkout',f'{repo.as_uri()}/trunk',wc)
for relative, contents in [
 ('src/teams/foo/seed', 'seed'),
 ('src/teams/foo/excluded.tmp', 'blocked'),
 ('src/teams/foobar/seed', 'must not publish'),
]:
 path=wc/relative
 path.parent.mkdir(parents=True, exist_ok=True)
 path.write_text(contents+'\n')
cmd('svn','add',wc/'src'); cmd('svn','commit',wc,'-m','seed r2')
cmd('git','init','--bare','--initial-branch=main',bare)
with socket.socket() as s: s.bind(('127.0.0.1',0)); port=s.getsockname()[1]
cfg.write_text(f"""
[daemon]
poll_interval_secs = 3
data_dir = "{data}"
log_level = "info"
[svn]
url = "{repo.as_uri()}"
username = "fixture"
[github]
api_url = "{base.as_uri()}"
repo = "local/fixture"
default_branch = "main"
[web]
listen = "127.0.0.1:{port}"
auth_mode = "simple"
admin_password_env = "REPOSYNC_ADMIN_PASSWORD"
session_secret_env = "REPOSYNC_SESSION_SECRET"
[identity]
email_domain = "example.invalid"
[sync]
mode = "direct"
auto_merge = false
sync_tags = false
""".strip()+'\n')
def call(path, method='GET', body=None, token=None):
 req=urllib.request.Request(f'http://127.0.0.1:{port}{path}',method=method,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json',**({'Authorization':'Bearer '+token} if token else {})})
 with urllib.request.urlopen(req,timeout=4) as r: return json.load(r)
p=None
def start(bin,tag):
 global p
 with (root/(tag+'.log')).open('wb') as out:
  p=subprocess.Popen([bin,'--config',str(cfg)],env=env,stdout=out,stderr=subprocess.STDOUT)
 return wait(lambda: call('/api/auth/login','POST',{'username':'admin','password':env['REPOSYNC_ADMIN_PASSWORD']})['token'],tag+' login')
def stop():
 global p
 if p and p.poll() is None:
  p.send_signal(signal.SIGTERM)
  try:p.wait(30)
  except subprocess.TimeoutExpired: p.kill(); p.wait(); raise
 p=None
def count(a,b): return int(cmd('git','--git-dir',bare,'rev-list','--count',f'{a}..{b}'))
def rejected(path, method, data, token, reason):
 try:
  call(path,method,data,token)
 except urllib.error.HTTPError as exc:
  actual=exc.read().decode()
  assert reason in actual, actual
 else:
  raise AssertionError(f"expected refusal: {reason}")
try:
 t=start(daemon,'app')
 pair=wait(lambda: (rows('select id from repositories') or [(None,)])[0][0],'Team registration')
 repo_api=f'/api/repos/{pair}'
 call(repo_api,'PUT',{'enabled':False},t)
 rule=json.dumps([{'strip_prefix':'src/teams/foo/'}])
 changes={'allowed_paths':'["src/teams/foo/"]','blocked_patterns':'["*.tmp"]','path_rewrite_rules':rule}
 rejected(repo_api,'PUT',{**changes,'allowed_paths':'["src/teams/"]'},t,'allowed scope extends outside')
 call(repo_api,'PUT',changes,t)
 assert rows('select path_rewrite_rules from repositories')==[(rule,)]
 rejected(repo_api,'PUT',{'enabled':True},t,'completed matching projected snapshot')
 rejected(repo_api+'/import','POST',{'import_mode':'full'},t,'requires a fresh projected snapshot')
 assert not cmd('git','--git-dir',bare,'for-each-ref')
 started=call(repo_api+'/import','POST',{'import_mode':'snapshot'},t)
 print('snapshot start',started,flush=True)
 def complete():
  status=call(repo_api+'/import/status',token=t)
  if status.get('phase')=='failed': raise RuntimeError(status)
  return status.get('phase')=='completed'
 wait(complete,'verified projected snapshot')
 h2=sha(); assert rev()==2
 assert cmd('git','--git-dir',bare,'ls-tree','-r','--name-only',h2)=='seed'
 assert gcat('seed')=='seed'
 call(repo_api,'PUT',{'enabled':True},t)
 (wc/'src/teams/foo/forward').write_text('one forward\n')
 (wc/'src/teams/foobar/hidden').write_text('never publish\n')
 cmd('svn','add',wc/'src/teams/foo/forward',wc/'src/teams/foobar/hidden')
 cmd('svn','commit',wc,'-m','forward mixed r3')
 wait(lambda:gcat('forward')=='one forward','projected SVN→Git')
 h3=sha(); assert count(h2,h3)==1
 assert cmd('git','--git-dir',bare,'ls-tree','-r','--name-only',h3)=='forward\nseed'
 cmd('git','clone',bare.as_uri(),clone)
 (clone/'forward').write_text('one reverse\n')
 cmd('git','-C',clone,'add','forward'); cmd('git','-C',clone,'commit','-m','reverse edit')
 cmd('git','-C',clone,'push','origin','main')
 h4=sha()
 wait(lambda:svncat('src/teams/foo/forward')=='one reverse','inverse Git→SVN')
 assert rev()==4 and svncat('src/teams/foobar/hidden')=='never publish'
 stop(); start(daemon,'restart')
 time.sleep(7); assert rev()==4 and sha()==h4
 # Copy a sibling directory, not merely a single file: source and
 # destination each have the same lexical prefix as the selected team.
 cmd('svn','update',wc)
 cmd('svn','copy',wc/'src/teams/foobar',wc/'src/teams/foobar-copy')
 cmd('svn','commit',wc,'-m','unrelated sibling copy r5')
 assert rev()==5
 zero_sql="select outcome,target_git_sha from canonical_mapping_outcomes where direction='svn_to_git' and source_key='svn:5'"
 wait(lambda:rows(zero_sql)==[('intentionally_filtered_no_target',None)],'unrelated copy checkpoint')
 assert sha()==h4
 (wc/'src/teams/foo/next').write_text('after sibling copy\n')
 cmd('svn','add',wc/'src/teams/foo/next')
 cmd('svn','commit',wc,'-m','selected r6 after sibling copy')
 wait(lambda:gcat('next')=='after sibling copy','selected revision after sibling-only copy')
 h6=sha()
 assert rev()==6 and count(h4,h6)==1
 assert cmd('git','--git-dir',bare,'ls-tree','-r','--name-only',h6)=='forward\nnext\nseed'
 before_boundary=rows("select last_svn_rev from repositories")
 before_mappings=rows("select direction,source_key,outcome,target_git_sha,target_svn_rev from canonical_mapping_outcomes order by id")
 assert ("git_to_svn","git:"+h4,"applied_and_verified",None,4) in before_mappings
 # A move from the declared rewrite prefix to another namespace is refused.
 cmd('svn','update',wc)
 cmd('svn','move',wc/'src/teams/foo/forward',wc/'src/teams/foobar/forward')
 cmd('svn','commit',wc,'-m','cross-boundary move held')
 assert rev()==7
 wait(lambda:'Team path rewriting holds SVN copies/renames' in (root/'restart.log').read_text(),'cross-boundary hold')
 assert sha()==h6 and rows('select last_svn_rev from repositories')==before_boundary
 assert rows('select direction,source_key,outcome,target_git_sha,target_svn_rev from canonical_mapping_outcomes order by id')==before_mappings
 print(json.dumps({'result':'pass','baseline':h2,'forward':h3,'reverse':h4,'selected_after_copy':h6,'svn_revisions':[2,3,4,5,6,7],'filtered_copy':5,'held':7}),flush=True)
except subprocess.CalledProcessError as exc:
 print(exc.output,flush=True); raise
finally:
 stop()
