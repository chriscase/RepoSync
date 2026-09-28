#![cfg(feature = "reliability-fixture")]
//! One complete old-installation -> copied migration -> Axum -> Node journey.
use std::{collections::{BTreeMap, HashMap}, fs, io::Write, os::unix::fs::PermissionsExt, path::Path, process::{Command, Stdio}, sync::{Arc, Mutex}};

use axum::{body::{to_bytes, Body}, http::{header, Method, Request}, Router};
use reposync_core::{
    config::{AppConfig, IdentityConfig},
    db::{candidate_authority::{advance_frontier, ResolvedTransition}, candidate_migration::CopySession, Database},
    git::GitClient, identity::IdentityMapper, import::ImportProgress,
    models::{Session, User},
    svn::SvnClient, sync_engine::SyncEngine,
};
use reposync_web::{api, AppState};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tower::Service;
use api::copy_inspection::InspectionContext;

fn copy(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry=entry.unwrap();let to=target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {copy(&entry.path(),&to)}
        else {fs::copy(entry.path(),to).unwrap();}
    }
}

fn tree(root: &Path) -> BTreeMap<String,String> {
    fn visit(root:&Path,p:&Path,out:&mut BTreeMap<String,String>) {
        for entry in fs::read_dir(p).unwrap() {
            let entry=entry.unwrap();let path=entry.path();
            if entry.file_type().unwrap().is_dir() {visit(root,&path,out)}
            else {out.insert(path.strip_prefix(root).unwrap().to_string_lossy().into_owned(),format!("{:o}:{}",fs::metadata(&path).unwrap().permissions().mode(),hex::encode(Sha256::digest(fs::read(path).unwrap()))));}
        }
    }
    let mut out=BTreeMap::new();visit(root,root,&mut out);out
}

fn old_topology() -> (TempDir,std::path::PathBuf,serde_json::Value) {
    let t=TempDir::new().unwrap();let root=t.path().join("generated");
    let generator=std::env::var("REPOSYNC_OLD_GENERATOR").expect("pinned old generator required");
    let output=Command::new(generator)
        .args(["generate_legacy_topology","--exact","--nocapture","--test-threads=1"])
        .env("REPOSYNC_OLD_TOPOLOGY_DIR",&root)
        .env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null")
        .env("GIT_AUTHOR_NAME","Fixture Developer").env("GIT_AUTHOR_EMAIL","fixture@example.invalid")
        .env("GIT_COMMITTER_NAME","Fixture Developer").env("GIT_COMMITTER_EMAIL","fixture@example.invalid")
        .output().unwrap();
    assert!(output.status.success(),"pinned old import failed: {}",String::from_utf8_lossy(&output.stderr));
    let provenance=String::from_utf8_lossy(&output.stderr).lines()
        .find_map(|line|line.strip_prefix("OLD_TOPOLOGY_EVIDENCE "))
        .map(|value|serde_json::from_str(value).unwrap()).unwrap();
    (t,root,provenance)
}

fn stage_structural_history(copy: &Path, baseline: &str) {
    let mut c=Connection::open(copy.join("reposync.db")).unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    let policy:String=c.query_row("SELECT policy_sha256 FROM pair_lineages WHERE repo_id='pair' AND generation=1",[],|r|r.get(0)).unwrap();
    for (id,d,pre,svn,git,kind,target_git,target_svn) in [
        ("http-in-applied","svn_to_git","svn:2".to_string(),Some(3),None,"applied_verified",Some("d".repeat(40)),None),
        ("http-in-empty","svn_to_git","svn:3".to_string(),Some(4),None,"empty_no_target",None,None),
        ("http-out-applied","git_to_svn",format!("git:{baseline}"),None,Some("e".repeat(40)),"applied_verified",None,Some(3)),
        ("http-out-empty","git_to_svn",format!("git:{}","e".repeat(40)),None,Some("f".repeat(40)),"semantic_no_delta",None,None),
    ] {
        advance_frontier(&mut c,&ResolvedTransition{id:id.into(),repo_id:"pair".into(),generation:1,direction:d.into(),predecessor_source_key:pre,source_svn_rev:svn,source_git_sha:git,outcome:kind.into(),target_git_sha:target_git,target_svn_rev:target_svn,projection_version:1,policy_sha256:policy.clone(),evidence_json:"{\"structural_fixture_only\":true}".into()}).unwrap();
    }
    for (id,d,key,pre,rev,git,kind) in [
        ("http-pending-in","svn_to_git","svn:5".to_string(),"svn:3".to_string(),Some(5),None,"pending"),
        ("http-unknown-in","svn_to_git","svn:8".to_string(),"svn:3".to_string(),Some(8),None,"effect_unknown"),
        ("http-pending-out","git_to_svn",format!("git:{}","a".repeat(40)),format!("git:{}","e".repeat(40)),None,Some("a".repeat(40)),"pending"),
    ] {c.execute("INSERT INTO pair_outcomes VALUES(?1,'pair',1,?2,?3,?4,?5,?6,?7,NULL,NULL,1,?8,'{\"structural_fixture_only\":true}')",rusqlite::params![id,d,key,pre,rev,git,kind,policy]).unwrap();}
    for (id,rev,sha,dir,interpretation) in [
        (200,3,Some("d".repeat(40)),"svn_to_git",Some("proved_typed_applied")),
        (201,4,None,"svn_to_git",Some("proved_typed_no_target")),
        (202,3,Some("e".repeat(40)),"git_to_svn",Some("proved_typed_applied")),
        (203,5,None,"svn_to_git",None),
        (204,6,Some("<img src=x onerror=alert(1)>".into()),"svn_to_git",None),
    ] {
        c.execute("INSERT INTO commit_map(id,svn_rev,git_sha,direction,synced_at,repo_id) VALUES(?1,?2,?3,?4,'t','pair')",rusqlite::params![id,rev,sha,dir]).unwrap();
        if let Some(i)=interpretation {c.execute("INSERT INTO legacy_evidence_links VALUES('pair',1,'commit_map',?1,?2)",rusqlite::params![id.to_string(),i]).unwrap();}
    }
    for (id,rev,repo) in [(-9_007_199_254_740_993i64,9,None),(-10,10,None),(0,11,None),(205,7,None),(206,6,Some("pair")),(9_007_199_254_740_993,12,None)] {
        c.execute("INSERT INTO commit_map(id,svn_rev,git_sha,direction,synced_at,repo_id) VALUES(?1,?2,NULL,'svn_to_git','t',?3)",rusqlite::params![id,rev,repo]).unwrap();
    }
}

fn stage_scope_canaries(copy: &Path) -> Vec<i64> {
    let c=Connection::open(copy.join("reposync.db")).unwrap();
    // These are display-only structural rows, never a newly proved remote effect.
    c.execute("UPDATE commit_map SET id=-7,svn_author='OWNERLESS_CANARY' WHERE id=0",[]).unwrap();
    c.execute("UPDATE commit_map SET svn_author='OWNERLESS_CANARY' WHERE id IN (-10,205)",[]).unwrap();
    for (id,repo,rev,author) in [
        (-8,"pair",7,"A_CANARY"), (0,"pair",13,"A_CANARY"),
        (-6,"pair_two",7,"B_CANARY"), (207,"pair_two",7,"B_CANARY"),
        (208,"pair",7,"A_CANARY"),
    ] {
        c.execute("INSERT INTO commit_map(id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id) VALUES(?1,?2,NULL,'svn_to_git','t',?3,'',?4)",rusqlite::params![id,rev,author,repo]).unwrap();
    }
    let ids=c.prepare("SELECT id FROM commit_map WHERE repo_id='pair' AND direction='svn_to_git' AND id > -11 ORDER BY id")
        .unwrap().query_map([],|r|r.get(0)).unwrap().collect::<rusqlite::Result<Vec<i64>>>().unwrap();
    ids
}

fn auth_state(t: &TempDir) -> Arc<AppState> {
    let config_text=format!("[daemon]\ndata_dir = {:?}\n[svn]\nurl = 'https://svn.test.invalid/repo'\nusername = 'fixture'\npassword_env = ''\n[github]\nrepo = 'test/repo'\ntoken_env = ''\n",t.path().display().to_string());
    let mut config:AppConfig=toml::from_str(&config_text).unwrap();
    config.web.admin_password=Some("fixture-password".into());
    let git_path=t.path().join("auth-only-git");git2::Repository::init(&git_path).unwrap();
    let web_db=Database::in_memory().unwrap();web_db.initialize().unwrap();
    let engine_db=Database::in_memory().unwrap();engine_db.initialize().unwrap();
    let engine=SyncEngine::new(config.clone(),engine_db,SvnClient::new("https://svn.test.invalid/repo","fixture",""),GitClient::new(&git_path).unwrap(),Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()));
    let (tx,_rx)=tokio::sync::mpsc::channel(1);let (broadcast,_)=tokio::sync::broadcast::channel(8);
    Arc::new(AppState{db:web_db,sync_engine:Arc::new(engine),config,sync_trigger:tx,ws_broadcast:broadcast,sessions:tokio::sync::RwLock::new(HashMap::new()),import_progress:Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),config_path:t.path().join("auth-config.toml"),prev_net_snapshot:Mutex::new(None),repo_import_progress:tokio::sync::RwLock::new(HashMap::new()),login_attempts:Mutex::new(HashMap::new()),import_handles:tokio::sync::Mutex::new(Vec::new())})
}

fn named_session(auth: &AppState, id: &str, token: &str, role: &str) {
    let now=chrono::Utc::now().to_rfc3339();
    auth.db.insert_user(&User{id:id.into(),username:id.into(),display_name:id.into(),email:format!("{id}@example.invalid"),password_hash:"fixture-only".into(),role:role.into(),enabled:true,created_at:now.clone(),updated_at:now.clone()}).unwrap();
    auth.db.insert_session(&Session{token:token.into(),user_id:id.into(),expires_at:(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),created_at:now}).unwrap();
}

#[derive(Serialize)]
struct Wire { label:String,status:u16,content_type:String,body:String }

async fn request(app:&Router,label:&str,method:Method,path:&str,token:Option<&str>) -> Wire {
    let mut builder=Request::builder().method(method).uri(path);
    if let Some(token)=token {builder=builder.header(header::AUTHORIZATION,format!("Bearer {token}"));}
    let mut app=app.clone();
    let response=app.call(builder.body(Body::empty()).unwrap()).await.unwrap();
    let status=response.status().as_u16();
    let content_type=response.headers().get(header::CONTENT_TYPE).and_then(|v|v.to_str().ok()).unwrap_or("").to_string();
    let body=String::from_utf8(to_bytes(response.into_body(),4*1024*1024).await.unwrap().to_vec()).unwrap();
    Wire{label:label.into(),status,content_type,body}
}

async fn denied(app:&Router,context:&InspectionContext,label:&str,method:Method,path:&str,token:Option<&str>,expected:u16) -> Wire {
    let before=context.reader_open_attempts();
    let wire=request(app,label,method,path,token).await;
    assert_eq!(wire.status,expected,"{label}: {}",wire.body);
    assert_eq!(context.reader_open_attempts(),before,"{label} opened candidate reader");
    assert!(!wire.body.contains("CANARY"),"{label} leaked fixture data");
    wire
}

fn scoped_node(mode:&str,input:&serde_json::Value) -> serde_json::Value {
    let script=std::env::var("REPOSYNC_SCOPED_CONSUMER").map(std::path::PathBuf::from)
        .unwrap_or_else(|_|Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/reliability-scoped-consumer.mjs"));
    let mut child=Command::new("node").arg(script).arg(mode).stdin(Stdio::piped())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("scoped Node consumer required");
    child.stdin.take().unwrap().write_all(&serde_json::to_vec(input).unwrap()).unwrap();
    let output=child.wait_with_output().unwrap();
    assert!(output.status.success(),"scoped Node consumer failed: {}",String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn copy_only_http_to_javascript_journey() {
    let (t,root,provenance)=old_topology();let source=root.join("install");
    let original_before=tree(&root);
    let copy_path=t.path().join("independent-copy");copy(&source,&copy_path);
    let mut session=CopySession::seal(&source,&copy_path).unwrap();
    session.qualify_imported_pair("pair",&root.join("svn-one"),&root.join("origin-one.git")).unwrap();
    session.qualify_imported_pair("pair_two",&root.join("svn-two"),&root.join("origin-two.git")).unwrap();
    assert!(session.qualify_imported_pair("pair_disabled",&root.join("svn-disabled"),&root.join("origin-disabled.git")).is_err());
    let report=session.migrate(14,&mut |_,_,_|Ok(())).unwrap();
    assert_eq!((report.starting_version,report.final_version),(12,14));
    stage_structural_history(&copy_path,provenance["pair"]["git_sha"].as_str().unwrap());
    let copy_before=tree(&copy_path);
    let auth=auth_state(&t);
    named_session(&auth,"fixture-operator","fixture-session","admin");
    let context=Arc::new(InspectionContext::new(Arc::new(session)));
    for repo in ["pair","pair_two","pair_disabled","missing"] { context.grant_read("fixture-operator",repo).await; }
    context.set_diagnostics("fixture-operator",true).await;
    let app=api::copy_inspection::routes(auth.clone(),context);
    let base="/__reliability/copy-read";
    let mut wire=Vec::new();
    for (label,path) in [
        ("lookup_in_mapped",format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=3")),
        ("lookup_out_mapped",format!("{base}/lookup?repository=pair&generation=1&direction=git_to_svn&source_git_sha={}","e".repeat(40))),
        ("lookup_in_no_target",format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=4")),
        ("lookup_out_no_target",format!("{base}/lookup?repository=pair&generation=1&direction=git_to_svn&source_git_sha={}","f".repeat(40))),
        ("lookup_missing_unknown",format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=8")),
        ("lookup_null",format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=5")),
        ("lookup_duplicate",format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=6")),
        ("lookup_ownerless",format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=7")),
        ("lookup_wrong_gen",format!("{base}/lookup?repository=pair&generation=2&direction=svn_to_git&source_svn_rev=3")),
        ("lookup_missing_gen",format!("{base}/lookup?repository=pair&direction=svn_to_git&source_svn_rev=3")),
        ("lookup_disabled",format!("{base}/lookup?repository=pair_disabled&generation=1&direction=svn_to_git&source_svn_rev=8")),
        ("lookup_missing_repo",format!("{base}/lookup?repository=missing&generation=1&direction=svn_to_git&source_svn_rev=8")),
        ("status_pair",format!("{base}/status?repository=pair&generation=1")),
        ("status_neighbor",format!("{base}/status?repository=pair_two&generation=1")),
        ("last_in",format!("{base}/last-emitted?repository=pair&generation=1&direction=svn_to_git")),
        ("last_out",format!("{base}/last-emitted?repository=pair&generation=1&direction=git_to_svn")),
        ("page_signed",format!("{base}/list?repository=pair&generation=1&direction=svn_to_git&after_id=-11&limit=2")),
        ("page_continue",format!("{base}/list?repository=pair&generation=1&direction=svn_to_git&after_id=199&limit=2")),
        ("page_empty",format!("{base}/list?repository=pair_two&generation=1&direction=git_to_svn&after_id=205&limit=2")),
        ("page_unsafe_positive",format!("{base}/list?repository=pair&generation=1&direction=svn_to_git&after_id=206&limit=1")),
        ("page_unsafe_negative",format!("{base}/list?repository=pair&generation=1&direction=svn_to_git&after_id=-9007199254740994&limit=1")),
    ] {wire.push(request(&app,label,Method::GET,&path,Some("fixture-session")).await);}
    wire.push(request(&app,"unauthenticated",Method::GET,&format!("{base}/status?repository=pair&generation=1"),None).await);
    wire.push(request(&app,"invalid_session",Method::GET,&format!("{base}/status?repository=pair&generation=1"),Some("wrong-session")).await);
    wire.push(request(&app,"invalid_source",Method::GET,&format!("{base}/lookup?repository=pair&generation=1&direction=git_to_svn&source_svn_rev=3"),Some("fixture-session")).await);
    wire.push(request(&app,"path_selection_refused",Method::GET,&format!("{base}/status?repository=pair&generation=1&path=/etc/passwd"),Some("fixture-session")).await);
    wire.push(request(&app,"post_refused",Method::POST,&format!("{base}/status?repository=pair&generation=1"),Some("fixture-session")).await);
    let unqualified_path=t.path().join("unqualified-copy");copy(&source,&unqualified_path);
    let mut unqualified=CopySession::seal(&source,&unqualified_path).unwrap();
    unqualified.disposition("pair","needs_reconciliation","fixture_only").unwrap();
    unqualified.migrate(14,&mut |_,_,_|Ok(())).unwrap();
    let unqualified_before=tree(&unqualified_path);
    let unqualified_context=Arc::new(InspectionContext::new(Arc::new(unqualified)));
    unqualified_context.grant_read("fixture-operator","pair").await;
    unqualified_context.set_diagnostics("fixture-operator",true).await;
    let unqualified_app=api::copy_inspection::routes(auth.clone(),unqualified_context);
    wire.push(request(&unqualified_app,"not_qualified",Method::GET,&format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=3"),Some("fixture-session")).await);
    let operational=Router::new().merge(api::sync_history::routes()).with_state(auth);
    wire.push(request(&operational,"default_candidate_absent",Method::GET,&format!("{base}/status?repository=pair&generation=1"),Some("fixture-session")).await);
    wire.push(request(&operational,"v12_commit_map",Method::GET,"/api/commit-map?repo_id=pair",Some("fixture-session")).await);
    wire.push(request(&operational,"v12_unauthenticated",Method::GET,"/api/commit-map?repo_id=pair",None).await);
    assert_eq!(tree(&root),original_before);
    assert_eq!(tree(&copy_path),copy_before);
    assert_eq!(tree(&unqualified_path),unqualified_before);
    if let Ok(path)=std::env::var("REPOSYNC_COPY_WIRE_OUTPUT") {
        fs::write(path,serde_json::to_vec_pretty(&wire).unwrap()).unwrap();
    }
    let script=std::env::var("REPOSYNC_COPY_CONSUMER").map(std::path::PathBuf::from).unwrap_or_else(|_|Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/reliability-copy-consumer.mjs"));
    let mut child=Command::new("node").arg(script).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("Node consumer required");
    child.stdin.take().unwrap().write_all(&serde_json::to_vec(&wire).unwrap()).unwrap();
    let output=child.wait_with_output().unwrap();
    assert!(output.status.success(),"JS consumer failed: {}",String::from_utf8_lossy(&output.stderr));
    let consumer:serde_json::Value=serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(consumer["status"],"PASS");
    eprintln!("RELIABILITY_EVIDENCE {}",serde_json::json!({"case":"S54_HTTP_COPY","old_generator_code":"87379741779a6259f7eeb52a68cc6f061174e5ef","original_import_svn":provenance["pair"]["svn_rev"],"copy_migration_versions":[12,14],"independent_copy":true,"in_process_axum_requests":wire.len(),"consumer":consumer,"source_config_refs_endpoints_and_copy_bytes_unchanged":true,"auth_state_separate":true,"default_operational_router_unchanged":true,"synthetic_later_history_not_external_effect_proof":true}));
}

#[tokio::test]
async fn scoped_read_authorization_journey() {
    let (t,root,provenance)=old_topology();
    let source=root.join("install");let original_before=tree(&root);
    let copy_path=t.path().join("scoped-copy");copy(&source,&copy_path);
    let mut session=CopySession::seal(&source,&copy_path).unwrap();
    session.qualify_imported_pair("pair",&root.join("svn-one"),&root.join("origin-one.git")).unwrap();
    session.qualify_imported_pair("pair_two",&root.join("svn-two"),&root.join("origin-two.git")).unwrap();
    assert!(session.qualify_imported_pair("pair_disabled",&root.join("svn-disabled"),&root.join("origin-disabled.git")).is_err());
    let report=session.migrate(14,&mut |_,_,_|Ok(())).unwrap();
    assert_eq!((report.starting_version,report.final_version),(12,14));
    stage_structural_history(&copy_path,provenance["pair"]["git_sha"].as_str().unwrap());
    let expected_ids=stage_scope_canaries(&copy_path);
    assert!(expected_ids.contains(&-8) && expected_ids.contains(&0));
    let copy_before=tree(&copy_path);

    let auth=auth_state(&t);
    for (id,token,role) in [("user-a","token-a","viewer"),("user-b","token-b","viewer"),
        ("user-c","token-c","viewer"),("operator","token-op","admin"),
        ("disabled","token-disabled","viewer"),("expired","token-expired","viewer")] {
        named_session(&auth,id,token,role);
    }
    let context=Arc::new(InspectionContext::new(Arc::new(session)));
    context.grant_read("user-a","pair").await;
    context.grant_read("user-a","pair_disabled").await;
    context.grant_read("user-b","pair_two").await;
    for repo in ["pair","pair_two"] {context.grant_read("operator",repo).await;}
    context.set_diagnostics("operator",true).await;
    context.grant_read("disabled","pair").await;
    context.grant_read("expired","pair").await;
    let app=api::copy_inspection::routes(auth.clone(),context.clone());
    let base="/__reliability/copy-read";
    let path=|repo:&str,op:&str| match op {
        "lookup" => format!("{base}/lookup?repository={repo}&generation=1&direction=svn_to_git&source_svn_rev=7"),
        "list" => format!("{base}/list?repository={repo}&generation=1&direction=svn_to_git&after_id=-11&limit=2"),
        "status" => format!("{base}/status?repository={repo}&generation=1"),
        "last-emitted" => format!("{base}/last-emitted?repository={repo}&generation=1&direction=svn_to_git"),
        _ => unreachable!(),
    };
    let mut wire=Vec::new();let mut denied_count=0;
    for (actor,token,repo,canary,hidden) in [("a","token-a","pair","A_CANARY","B_CANARY"),
        ("b","token-b","pair_two","B_CANARY","A_CANARY")] {
        for op in ["lookup","list","status","last-emitted"] {
            let label=format!("{actor}_{op}");
            let response=request(&app,&label,Method::GET,&path(repo,op),Some(token)).await;
            assert_eq!(response.status,200,"{label}: {}",response.body);
            assert!(response.body.contains("reposync.copy_read.scoped.v1"));
            assert!(!response.body.contains("OWNERLESS_CANARY") && !response.body.contains(hidden));
            if op=="lookup" {assert!(response.body.contains(canary));}
            wire.push(response);
        }
    }
    for (actor,token,other) in [("a","token-a","pair_two"),("b","token-b","pair")] {
        for op in ["lookup","list","status","last-emitted"] {
            let label=format!("{actor}_denied_{op}");
            wire.push(denied(&app,&context,&label,Method::GET,&path(other,op),Some(token),403).await);
            denied_count+=1;
        }
    }
    for (label,token,repo) in [("no_grant","token-c","pair"),("unknown_repo","token-a","pair_other")]
    { wire.push(denied(&app,&context,label,Method::GET,&path(repo,"status"),Some(token),403).await);denied_count+=1; }
    let scoped_missing=request(&app,"a_ownerless_only",Method::GET,
        &format!("{base}/lookup?repository=pair&generation=1&direction=svn_to_git&source_svn_rev=9"),Some("token-a")).await;
    assert_eq!(scoped_missing.status,200);
    assert!(!scoped_missing.body.contains("OWNERLESS_CANARY"));
    wire.push(scoped_missing);
    let diagnostic=request(&app,"operator_diagnostic",Method::GET,&path("pair","lookup"),Some("token-op")).await;
    assert_eq!(diagnostic.status,200);
    assert!(diagnostic.body.contains("reposync.copy_read.v1") && diagnostic.body.contains("OWNERLESS_CANARY"));
    wire.push(diagnostic);
    let diagnostic_list=request(&app,"operator_diagnostic_list",Method::GET,&path("pair","list"),Some("token-op")).await;
    assert_eq!(diagnostic_list.status,200);
    assert!(diagnostic_list.body.contains("OWNERLESS_CANARY"));
    wire.push(diagnostic_list);

    // Node decodes each real HTTP response and constructs the next request
    // from its returned cursor. Rust dispatches precisely that request.
    let mut next_path=Some(path("pair","list"));
    let mut visited=Vec::new();let mut pages=0;
    while let Some(uri)=next_path.take() {
        assert!(pages<20,"pagination did not terminate");
        let label=format!("scoped_page_{pages}");
        let page=request(&app,&label,Method::GET,&uri,Some("token-a")).await;
        assert_eq!(page.status,200,"{label}: {}",page.body);
        let decision=scoped_node("next",&serde_json::json!({"wire":&page,"base":base}));
        let ids=decision["ids"].as_array().unwrap().iter().map(|v|v.as_i64().unwrap()).collect::<Vec<_>>();
        visited.extend(ids);
        next_path=decision["next_path"].as_str().map(str::to_string);
        wire.push(page);pages+=1;
    }
    assert!(pages>=3);
    assert_eq!(visited,expected_ids,"response-driven permitted page IDs");

    let wrong_gen=request(&app,"a_wrong_generation",Method::GET,
        &format!("{base}/status?repository=pair&generation=99"),Some("token-a")).await;
    assert_eq!(wrong_gen.status,200);assert!(wrong_gen.body.contains("missing_generation"));wire.push(wrong_gen);
    let missing_gen=request(&app,"a_missing_generation",Method::GET,
        &format!("{base}/status?repository=pair"),Some("token-a")).await;
    assert_eq!(missing_gen.status,200);assert!(missing_gen.body.contains("missing_generation"));wire.push(missing_gen);
    let disabled_repo=request(&app,"a_disabled_repository",Method::GET,
        &path("pair_disabled","status"),Some("token-a")).await;
    assert_eq!(disabled_repo.status,200);assert!(disabled_repo.body.contains("disabled"));wire.push(disabled_repo);

    let unqualified_path=t.path().join("scope-unqualified-copy");copy(&source,&unqualified_path);
    let mut unqualified=CopySession::seal(&source,&unqualified_path).unwrap();
    unqualified.disposition("pair","needs_reconciliation","fixture_only").unwrap();
    unqualified.migrate(14,&mut |_,_,_|Ok(())).unwrap();
    let unqualified_before=tree(&unqualified_path);
    let other_context=Arc::new(InspectionContext::new(Arc::new(unqualified)));
    let other_app=api::copy_inspection::routes(auth.clone(),other_context.clone());
    wire.push(denied(&other_app,&other_context,"other_copy_denied",Method::GET,
        &path("pair","status"),Some("token-a"),403).await);denied_count+=1;
    other_context.grant_read("user-a","pair").await;
    let unqualified_wire=request(&other_app,"other_copy_unqualified",Method::GET,
        &path("pair","status"),Some("token-a")).await;
    assert_eq!(unqualified_wire.status,200);assert!(unqualified_wire.body.contains("not_qualified"));wire.push(unqualified_wire);

    context.revoke_read("user-a","pair").await;
    wire.push(denied(&app,&context,"grant_revoked",Method::GET,&path("pair","status"),Some("token-a"),403).await);denied_count+=1;
    context.grant_read("user-a","pair").await;
    context.set_diagnostics("operator",false).await;
    let demoted=request(&app,"diagnostic_revoked",Method::GET,&path("pair","lookup"),Some("token-op")).await;
    assert_eq!(demoted.status,200);assert!(demoted.body.contains("reposync.copy_read.scoped.v1"));
    assert!(!demoted.body.contains("OWNERLESS_CANARY"));wire.push(demoted);
    context.set_diagnostics("operator",true).await;
    auth.db.update_user("operator","operator","operator@example.invalid","viewer",true).unwrap();
    let role_demoted=request(&app,"role_downgraded",Method::GET,&path("pair","lookup"),Some("token-op")).await;
    assert_eq!(role_demoted.status,200);assert!(role_demoted.body.contains("reposync.copy_read.scoped.v1"));
    assert!(!role_demoted.body.contains("OWNERLESS_CANARY"));wire.push(role_demoted);
    auth.db.delete_session("token-op").unwrap();
    auth.sessions.write().await.insert("token-op".into(),chrono::Utc::now()+chrono::Duration::hours(1));
    wire.push(denied(&app,&context,"operator_logged_out",Method::GET,&path("pair","lookup"),Some("token-op"),401).await);denied_count+=1;
    auth.db.disable_user("disabled").unwrap();
    wire.push(denied(&app,&context,"disabled_principal",Method::GET,&path("pair","status"),Some("token-disabled"),401).await);denied_count+=1;
    auth.db.delete_session("token-expired").unwrap();
    auth.db.insert_session(&Session{token:"token-expired".into(),user_id:"expired".into(),
        expires_at:(chrono::Utc::now()-chrono::Duration::hours(1)).to_rfc3339(),created_at:chrono::Utc::now().to_rfc3339()}).unwrap();
    auth.sessions.write().await.insert("token-expired".into(),chrono::Utc::now()+chrono::Duration::hours(1));
    wire.push(denied(&app,&context,"expired_named_session",Method::GET,&path("pair","status"),Some("token-expired"),401).await);denied_count+=1;
    auth.db.insert_session(&Session{token:"ghost-token".into(),user_id:"missing-user".into(),
        expires_at:(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),created_at:chrono::Utc::now().to_rfc3339()}).unwrap();
    auth.sessions.write().await.insert("ghost-token".into(),chrono::Utc::now()+chrono::Duration::hours(1));
    wire.push(denied(&app,&context,"missing_principal",Method::GET,&path("pair","status"),Some("ghost-token"),401).await);denied_count+=1;
    auth.db.insert_session(&Session{token:"malformed-expiry".into(),user_id:"user-a".into(),
        expires_at:"zzzz".into(),created_at:chrono::Utc::now().to_rfc3339()}).unwrap();
    wire.push(denied(&app,&context,"malformed_expiry",Method::GET,&path("pair","status"),Some("malformed-expiry"),401).await);denied_count+=1;
    context.set_grants_available(false);
    wire.push(denied(&app,&context,"policy_failure",Method::GET,&path("pair","status"),Some("token-a"),403).await);denied_count+=1;
    context.set_grants_available(true);
    wire.push(denied(&app,&context,"legacy_token_refused",Method::GET,&path("pair","status"),Some("token-op"),401).await);denied_count+=1;
    wire.push(denied(&app,&context,"anonymous",Method::GET,&path("pair","status"),None,401).await);denied_count+=1;
    wire.push(denied(&app,&context,"query_path_refused",Method::GET,
        &format!("{base}/status?repository=pair&generation=1&path=/etc/passwd"),Some("token-a"),400).await);denied_count+=1;
    wire.push(denied(&app,&context,"query_principal_refused",Method::GET,
        &format!("{base}/status?repository=pair&generation=1&user_id=operator"),Some("token-a"),400).await);denied_count+=1;
    wire.push(denied(&app,&context,"direction_refused",Method::GET,
        &format!("{base}/last-emitted?repository=pair&generation=1&direction=other"),Some("token-a"),400).await);denied_count+=1;
    wire.push(denied(&app,&context,"method_refused",Method::POST,&path("pair","status"),Some("token-a"),405).await);denied_count+=1;

    // The unchanged operational v12 handler receives populated, complete old
    // fields. Its auth store is separate from both migrated copies.
    {
        let c=auth.db.conn();
        for (id,repo,rev,sha,author) in [(501,"pair",31,"a".repeat(40),"Old A"),
            (502,"pair",32,"b".repeat(40),"Old B"),(503,"pair_two",33,"c".repeat(40),"Old neighbor")] {
            c.execute("INSERT INTO commit_map(id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id) VALUES(?1,?2,?3,'svn_to_git','2020-01-01T00:00:00Z',?4,'old-git-author',?5)",rusqlite::params![id,rev,sha,author,repo]).unwrap();
        }
    }
    let operational=Router::new().merge(api::sync_history::routes()).with_state(auth.clone());
    wire.push(request(&operational,"v12_populated",Method::GET,"/api/commit-map?repo_id=pair&limit=2",Some("token-a")).await);
    wire.push(request(&operational,"v12_filtered",Method::GET,"/api/commit-map?repo_id=pair&limit=1",Some("token-a")).await);
    wire.push(request(&operational,"v12_unfiltered",Method::GET,"/api/commit-map?limit=3",Some("token-a")).await);
    wire.push(request(&operational,"v12_populated_unauthenticated",Method::GET,"/api/commit-map?repo_id=pair",None).await);
    wire.push(request(&operational,"default_candidate_absent_scoped",Method::GET,
        &path("pair","status"),Some("token-a")).await);
    auth.db.disable_user("user-b").unwrap();
    wire.push(denied(&app,&context,"b_disabled_after_success",Method::GET,
        &path("pair_two","status"),Some("token-b"),401).await);denied_count+=1;
    auth.db.delete_session("token-a").unwrap();
    auth.db.insert_session(&Session{token:"token-a".into(),user_id:"user-a".into(),
        expires_at:(chrono::Utc::now()-chrono::Duration::seconds(1)).to_rfc3339(),created_at:chrono::Utc::now().to_rfc3339()}).unwrap();
    wire.push(denied(&app,&context,"a_expired_after_success",Method::GET,
        &path("pair","status"),Some("token-a"),401).await);denied_count+=1;

    assert_eq!(tree(&root),original_before);
    assert_eq!(tree(&copy_path),copy_before);
    assert_eq!(tree(&unqualified_path),unqualified_before);
    if let Ok(path)=std::env::var("REPOSYNC_SCOPED_WIRE_OUTPUT") {
        fs::write(path,serde_json::to_vec_pretty(&wire).unwrap()).unwrap();
    }
    let consumer=scoped_node("verify",&serde_json::json!({"wires":wire,"expected_ids":expected_ids}));
    assert_eq!(consumer["status"],"PASS");
    eprintln!("RELIABILITY_EVIDENCE {}",serde_json::json!({"case":"S54_SCOPED_HTTP","old_generator_code":"87379741779a6259f7eeb52a68cc6f061174e5ef",
        "copy_migration_versions":[12,14],"independent_copies":2,"actor_route_matrix":"a_and_b_each_four_allowed_and_four_cross_denied",
        "denied_without_reader_open":denied_count,"reader_open_attempts":context.reader_open_attempts(),
        "diagnostic_visibility_and_revocation":true,"scoped_ownerless_neighbor_nondisclosure":true,
        "response_driven_pages":pages,"permitted_ids":visited,"consumer":consumer,
        "populated_v12_actual_handler":true,"original_copy_config_refs_endpoints_unchanged":true,
        "later_structural_history_not_external_effect_proof":true}));
}
