#![cfg(feature = "reliability-fixture")]
//! One complete old-installation -> copied migration -> Axum -> Node journey.
use std::{collections::{BTreeMap, HashMap}, fs, io::Write, os::unix::fs::PermissionsExt, path::Path, process::{Command, Stdio}, sync::{Arc, Mutex}};

use axum::{body::{to_bytes, Body}, http::{header, Method, Request}, Router};
use reposync_core::{
    config::{AppConfig, IdentityConfig},
    db::{candidate_authority::{advance_frontier, ResolvedTransition}, candidate_migration::CopySession, Database},
    git::GitClient, identity::IdentityMapper, import::ImportProgress,
    svn::SvnClient, sync_engine::SyncEngine,
};
use reposync_web::{api, AppState};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tower::Service;

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
    auth.sessions.write().await.insert("fixture-session".into(),chrono::Utc::now()+chrono::Duration::hours(1));
    let app=api::copy_inspection::routes(auth.clone(),Arc::new(session));
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
    let unqualified_app=api::copy_inspection::routes(auth.clone(),Arc::new(unqualified));
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
