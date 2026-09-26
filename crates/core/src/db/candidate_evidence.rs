//! One pinned evidence vocabulary shared with the read-only inventory. Local
//! inventory labels are not endpoint proof; this decision is an admission gate.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
#[derive(Deserialize)]
struct Vocabulary {
    scoped_svn_prefix:String, scoped_git_prefix:String, no_target_prefix:String,
    baseline_prefix:String, unsupported_aggregate_prefix:String, unknown_effect_prefix:String,
    svn_watermark_sources:Vec<String>,
}
#[derive(Debug,Serialize)]
pub struct AdmissionDecision {
    pub disposition:String,
    pub reasons:Vec<String>,
    pub global_references_are_not_authority:bool,
}
pub fn imported_evidence(c:&Connection,repo:&str,rev:i64,sha:&str)->Result<AdmissionDecision> {
    let v:Vocabulary=serde_json::from_str(include_str!("../../../../docs/reliability/legacy-evidence-vocabulary.json"))?;
    let keys=c.prepare("SELECT key,value FROM kv_state ORDER BY key")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<std::collections::BTreeMap<_,_>>>()?;
    let ids=c.prepare("SELECT id FROM repositories")?.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut reasons=Vec::new();
    if keys.get(&format!("{}{repo}",v.scoped_git_prefix)).map(String::as_str)!=Some(sha) {reasons.push("outgoing_cursor_copies_differ_or_missing".into());}
    if keys.get(&format!("{}{repo}",v.scoped_svn_prefix)).is_some_and(|s|s!=&rev.to_string()) {reasons.push("scoped_incoming_cursor_disagrees".into());}
    let owned_receipt=keys.keys().any(|key| {
        ids.iter().filter(|id|key.starts_with(&format!("{}{id}_",v.no_target_prefix))).max_by_key(|id|id.len()).is_some_and(|id|id==repo)
    });
    if owned_receipt || keys.contains_key(&format!("{}{repo}",v.baseline_prefix)) || keys.contains_key(&format!("{}{repo}",v.unsupported_aggregate_prefix)) {reasons.push("owned_receipt_topology_unsupported".into());}
    for source in &v.svn_watermark_sources {
        let value:Option<String>=c.query_row("SELECT value FROM watermarks WHERE source=?1",[source],|r|r.get(0)).optional()?;
        if value.is_some_and(|x|x!=rev.to_string()) {reasons.push(format!("unowned_{source}_watermark_revision_unproved"));}
    }
    let progress=c.prepare("SELECT repo_id,phase,current_rev,total_revs FROM import_progress ORDER BY id")?.query_map([],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (owner,phase,current,total) in progress {
        if owner.as_deref()==Some(repo) || owner.is_none() {
            if current!=rev || total!=rev || phase!="completed" {reasons.push(if owner.is_some(){"owned_import_progress_conflicts"}else{"unowned_import_progress_requires_reconciliation"}.into());}
        }
    }
    let unknown=keys.contains_key(&format!("{}{repo}",v.unknown_effect_prefix));
    if unknown {reasons.push("owned_external_effect_unknown".into());}
    let enabled:i64=c.query_row("SELECT enabled FROM repositories WHERE id=?1",[repo],|r|r.get(0))?;
    let disposition=if enabled==0 {"not_qualified"}else if unknown {"external_effect_unknown"}else if reasons.is_empty(){"qualified"}else{"needs_reconciliation"};
    Ok(AdmissionDecision{disposition:disposition.into(),reasons,global_references_are_not_authority:true})
}
