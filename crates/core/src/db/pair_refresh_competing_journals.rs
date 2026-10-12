//! Global admission checks include records for removed/unregistered IDs.
//! Completed legacy history is allowed; uncertain history remains retained.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;

use crate::errors::DatabaseError;

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("pair refresh competing journal refused: {detail}"))
}

// Legacy documents permit omitted default fields. Preserve that compatibility
// while rejecting duplicate/unknown fields rather than losing their evidence.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate fields")
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, UniqueValue(value))) =
                    map.next_entry::<String, UniqueValue>()?
                {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate field"));
                    }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

fn known_projection(raw: &Value, typed: &Value) -> bool {
    match (raw, typed) {
        (Value::Object(raw), Value::Object(typed)) => raw.iter().all(|(key, value)| {
            typed
                .get(key)
                .is_some_and(|known| known_projection(value, known))
        }),
        (Value::Array(raw), Value::Array(typed)) => {
            raw.len() == typed.len()
                && raw
                    .iter()
                    .zip(typed)
                    .all(|(raw, typed)| known_projection(raw, typed))
        }
        _ => raw == typed,
    }
}

fn typed<T: for<'de> Deserialize<'de> + Serialize>(raw: &str) -> Result<Value, DatabaseError> {
    let unique: UniqueValue =
        serde_json::from_str(raw).map_err(|_| refused("ambiguous competing document"))?;
    let document: T = serde_json::from_value(unique.0.clone())
        .map_err(|_| refused("corrupt competing document"))?;
    let normalized = serde_json::to_value(document).map_err(|_| refused("competing encoding"))?;
    if !known_projection(&unique.0, &normalized) {
        return Err(refused("unknown competing document fields"));
    }
    Ok(normalized)
}

pub(crate) fn require_globally_quiet(tx: &Connection) -> Result<(), DatabaseError> {
    for (family, kind) in [
        ("import_operation_", "import_operation_v1:"),
        ("git_to_svn_commit_", "git_to_svn_commit_v1:"),
        ("svn_to_git_push_", "svn_to_git_push_v1:"),
        ("team_cycle_mapping_", "team_cycle_mapping_v1:"),
        ("late_pair_publish_", "late_pair_publish_v1:"),
        ("managed_remove_", "managed_remove_v1:"),
    ] {
        let mut query =
            tx.prepare("SELECT key,value FROM kv_state WHERE substr(key,1,?1)=?2 ORDER BY key")?;
        let rows = query
            .query_map(params![family.len(), family], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut documents = BTreeMap::new();
        for (key, raw) in &rows {
            let suffix = key
                .strip_prefix(kind)
                .ok_or_else(|| refused("unknown competing journal version"))?;
            if let Some(id) = suffix.strip_prefix("document:") {
                let document = match kind {
                    "import_operation_v1:" => {
                        typed::<super::import_operations::ImportOperation>(raw)?
                    }
                    "git_to_svn_commit_v1:" => {
                        typed::<super::svn_commit_operations::SvnCommitOperation>(raw)?
                    }
                    "svn_to_git_push_v1:" => {
                        typed::<super::git_push_operations::GitPushOperation>(raw)?
                    }
                    "team_cycle_mapping_v1:" => typed::<
                        super::team_cycle_mapping_operations::TeamCycleMappingOperation,
                    >(raw)?,
                    "late_pair_publish_v1:" => {
                        typed::<super::late_pair_publish_operations::LatePairPublishOperation>(raw)?
                    }
                    "managed_remove_v1:" => {
                        typed::<super::managed_remove::ManagedRemoveOperation>(raw)?
                    }
                    _ => return Err(refused("unsupported competing family")),
                };
                let owner_key = if kind == "late_pair_publish_v1:" {
                    "parent_repo_id"
                } else {
                    "repo_id"
                };
                let owner = document[owner_key]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| refused("competing owner is missing"))?;
                let operation_type = document["operation_type"].as_str().unwrap_or_default();
                let expected_type = match kind {
                    "import_operation_v1:" => {
                        ["full_import", "snapshot_import"].contains(&operation_type)
                    }
                    "git_to_svn_commit_v1:" => operation_type == "git_to_svn_commit",
                    "svn_to_git_push_v1:" => operation_type == "svn_to_git_push",
                    "team_cycle_mapping_v1:" => operation_type == "team_cycle_mapping",
                    "late_pair_publish_v1:" => operation_type == "late_pair_publish",
                    "managed_remove_v1:" => operation_type == "managed_remove",
                    _ => false,
                };
                if document["version"] != 1
                    || document["id"] != id
                    || id.is_empty()
                    || !expected_type
                {
                    return Err(refused("competing identity/type differs"));
                }
                // Legacy failed/reconciliation records do not prove absence of
                // publication. A cancelled import with no remaining intent or
                // pending mappings is the separately supported safe terminal.
                let safe_cancel = kind == "import_operation_v1:"
                    && document["state"] == "cancelled"
                    && document["intended_ref"].is_null()
                    && document["intended_git_sha"].is_null()
                    && document
                        .get("pending_commit_maps")
                        .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty));
                if document["state"] != "completed" && !safe_cancel {
                    return Err(refused("unfinished or uncertain competing history"));
                }
                documents.insert(id.to_owned(), (owner.to_owned(), document));
            } else if !(suffix.starts_with("latest:")
                || suffix.starts_with("active:")
                || (kind == "managed_remove_v1:" && suffix.starts_with("tombstone:")))
            {
                return Err(refused("unknown competing journal record"));
            }
        }
        for (key, raw) in &rows {
            let suffix = key
                .strip_prefix(kind)
                .ok_or_else(|| refused("unknown competing version"))?;
            if suffix.starts_with("active:") {
                return Err(refused("competing durable active pointer"));
            }
            if let Some(owner) = suffix.strip_prefix("latest:") {
                if !documents
                    .get(raw)
                    .is_some_and(|(document_owner, _)| document_owner == owner)
                {
                    return Err(refused("orphan or contradictory latest pointer"));
                }
            }
            if let Some(owner) = suffix.strip_prefix("tombstone:") {
                let tombstone = typed::<super::managed_remove::RemovalTombstone>(raw)?;
                let id = tombstone["operation_id"]
                    .as_str()
                    .ok_or_else(|| refused("tombstone operation missing"))?;
                if tombstone["version"] != 1
                    || tombstone["repo_id"] != owner
                    || !documents.get(id).is_some_and(|(document_owner, document)| {
                        document_owner == owner && document["state"] == "completed"
                    })
                {
                    return Err(refused("orphan or contradictory removal tombstone"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed_import() -> super::super::import_operations::ImportOperation {
        serde_json::from_value(serde_json::json!({
            "version":1,"id":"retained-import","repo_id":"deleted-registration",
            "operation_type":"full_import","initiator_id":"admin","request_id":"request",
            "target_fingerprint":"a".repeat(64),"created_at":"2026-10-12T12:00:00Z",
            "updated_at":"2026-10-12T12:00:01Z","state":"completed","cancel_requested":false
        }))
        .unwrap()
    }

    fn insert(tx: &Connection, key: &str, value: &str) {
        tx.execute(
            "INSERT OR REPLACE INTO kv_state(key,value,updated_at) VALUES(?1,?2,'fixture')",
            params![key, value],
        )
        .unwrap();
    }

    #[test]
    fn completed_history_for_deleted_registration_is_allowed_without_adoption_or_repair() {
        let db = super::super::Database::in_memory().unwrap();
        db.initialize().unwrap();
        let conn = db.conn();
        let document = serde_json::to_string(&completed_import()).unwrap();
        insert(
            &conn,
            "import_operation_v1:document:retained-import",
            &document,
        );
        insert(
            &conn,
            "import_operation_v1:latest:deleted-registration",
            "retained-import",
        );
        require_globally_quiet(&conn).unwrap();
        let retained: String = conn.query_row("SELECT value FROM kv_state WHERE key='import_operation_v1:document:retained-import'", [], |row| row.get(0)).unwrap();
        assert_eq!(retained, document);
        let registrations: i64 = conn
            .query_row("SELECT count(*) FROM repositories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(registrations, 0);
    }

    #[test]
    fn orphan_unfinished_historical_document_is_not_hidden_by_a_completed_latest_pointer() {
        let db = super::super::Database::in_memory().unwrap();
        db.initialize().unwrap();
        let conn = db.conn();
        let completed = completed_import();
        insert(
            &conn,
            "import_operation_v1:document:retained-import",
            &serde_json::to_string(&completed).unwrap(),
        );
        insert(
            &conn,
            "import_operation_v1:latest:deleted-registration",
            "retained-import",
        );
        let mut uncertain = completed;
        uncertain.id = "older-uncertain".into();
        uncertain.state =
            super::super::import_operations::ImportOperationState::ReconciliationRequired;
        uncertain.intended_git_sha = Some("b".repeat(40));
        uncertain.intended_ref = Some("refs/heads/main".into());
        let raw = serde_json::to_string(&uncertain).unwrap();
        insert(&conn, "import_operation_v1:document:older-uncertain", &raw);
        assert!(require_globally_quiet(&conn).is_err());
        let retained: String = conn.query_row("SELECT value FROM kv_state WHERE key='import_operation_v1:document:older-uncertain'", [], |row| row.get(0)).unwrap();
        assert_eq!(retained, raw);
    }

    #[test]
    fn omitted_legacy_defaults_are_supported_but_unknown_and_duplicate_evidence_refuses() {
        let db = super::super::Database::in_memory().unwrap();
        db.initialize().unwrap();
        let conn = db.conn();
        let mut value = serde_json::to_value(completed_import()).unwrap();
        value.as_object_mut().unwrap().remove("processed_revisions");
        value.as_object_mut().unwrap().remove("resume_authorized");
        let raw = serde_json::to_string(&value).unwrap();
        insert(&conn, "import_operation_v1:document:retained-import", &raw);
        require_globally_quiet(&conn).unwrap();
        value["unknown_publication_evidence"] = serde_json::json!(true);
        insert(
            &conn,
            "import_operation_v1:document:retained-import",
            &serde_json::to_string(&value).unwrap(),
        );
        assert!(require_globally_quiet(&conn).is_err());
        let duplicate = format!("{{\"state\":\"reconciliation_required\",{}", &raw[1..]);
        insert(
            &conn,
            "import_operation_v1:document:retained-import",
            &duplicate,
        );
        assert!(require_globally_quiet(&conn).is_err());
    }

    #[test]
    fn safe_cancelled_import_history_needs_no_intent_and_no_pending_correspondence() {
        let db = super::super::Database::in_memory().unwrap();
        db.initialize().unwrap();
        let conn = db.conn();
        let mut op = completed_import();
        op.state = super::super::import_operations::ImportOperationState::Cancelled;
        insert(
            &conn,
            "import_operation_v1:document:retained-import",
            &serde_json::to_string(&op).unwrap(),
        );
        require_globally_quiet(&conn).unwrap();
        op.pending_commit_maps
            .push(super::super::import_operations::PendingImportCommitMap {
                svn_rev: 4,
                git_sha: "b".repeat(40),
                svn_author: "fixture".into(),
                git_author: "fixture".into(),
            });
        insert(
            &conn,
            "import_operation_v1:document:retained-import",
            &serde_json::to_string(&op).unwrap(),
        );
        assert!(require_globally_quiet(&conn).is_err());
    }
}
