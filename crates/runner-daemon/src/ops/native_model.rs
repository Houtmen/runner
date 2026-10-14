use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::{repo, AppCore};

const MAX_HEADER_BYTES: u64 = 1024 * 1024;
const MAX_TAIL_BYTES: u64 = 1024 * 1024;

fn codex_model(path: &Path, native_key: &str) -> Result<String> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
        return Err(Error::msg("select the native Codex .jsonl rollout file"));
    }
    if uuid::Uuid::parse_str(native_key).is_err() {
        return Err(Error::msg("captured Codex session ID is invalid"));
    }
    if !std::fs::metadata(path)?.is_file() {
        return Err(Error::msg("native transcript must be a regular file"));
    }
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::msg("native transcript must be a regular file"));
    }
    let mut header = String::new();
    BufReader::new((&mut file).take(MAX_HEADER_BYTES)).read_line(&mut header)?;
    let header: Value = serde_json::from_str(&header)?;
    if header["type"] != "session_meta" || header["payload"]["id"].as_str() != Some(native_key) {
        return Err(Error::msg(
            "transcript does not belong to this native session",
        ));
    }
    let offset = metadata.len().saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(offset))?;
    let mut tail = Vec::new();
    file.take(MAX_TAIL_BYTES).read_to_end(&mut tail)?;
    let mut model = None;
    for (index, line) in tail.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if offset > 0 && index == 0 {
            continue;
        }
        if line.last() != Some(&b'\n') {
            continue;
        }
        let record: Value = serde_json::from_slice(line)?;
        if record["type"] == "turn_context" {
            model = record["payload"]["model"]
                .as_str()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned);
        }
    }
    model.ok_or_else(|| {
        Error::msg("no durable model record in the bounded transcript tail; model unchanged")
    })
}

pub fn session_sync_model(core: &AppCore, session_id: &str, transcript: &Path) -> Result<Value> {
    let _claim = core
        .sessions
        .try_claim_resume(session_id)
        .ok_or_else(|| Error::msg("session is being resumed; retry later"))?;
    let mut conn = core.db.get()?;
    let row = repo::session::get_row(&conn, session_id)?
        .ok_or_else(|| Error::msg("session not found"))?;
    if row.agent_runtime.as_deref() != Some("codex") {
        return Err(Error::msg(
            "native model synchronization is verified only for Codex; no model changed",
        ));
    }
    if row.archived_at.is_some() {
        return Err(Error::msg(
            "unarchive the session before synchronizing its model",
        ));
    }
    let native_key = row
        .agent_session_key
        .as_deref()
        .ok_or_else(|| Error::msg("native session ID has not been captured"))?;
    let observed = codex_model(transcript, native_key)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = repo::session::get_row(&tx, session_id)?
        .ok_or_else(|| Error::msg("session disappeared"))?;
    if current.agent_session_key != row.agent_session_key
        || current.agent_runtime != row.agent_runtime
        || current.archived_at.is_some()
    {
        return Err(Error::msg(
            "session identity changed while reading its transcript; retry",
        ));
    }
    let mut definition = current
        .launch_definition
        .ok_or_else(|| Error::msg("session has no recorded launch definition"))?;
    definition.model = Some(observed.clone());
    repo::session::set_launch(&tx, session_id, "codex", &definition)?;
    tx.commit()?;
    core.events.emit(
        "session/updated",
        &json!({"session_id":session_id,"mission_id":current.mission_id}),
    );
    Ok(
        json!({"session_id":session_id, "native_session_id":native_key, "runtime":"codex", "model":observed, "source":"native_turn_context", "history":"same_native_session"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(path: &Path, key: &str, content: &str) {
        std::fs::write(
            path,
            format!(
                "{}\n{content}",
                json!({"type":"session_meta","payload":{"id":key}})
            ),
        )
        .unwrap();
    }

    #[test]
    fn latest_durable_model_is_read_without_assuming_partial_records_or_other_sessions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout.jsonl");
        transcript(&path, "00000000-0000-4000-8000-000000000001", "{\"type\":\"turn_context\",\"payload\":{\"model\":\"first\"}}\n{\"type\":\"turn_context\",\"payload\":{\"model\":\"second\"}}\n{\"type\":\"turn_context\",\"payload\":");
        assert_eq!(
            codex_model(&path, "00000000-0000-4000-8000-000000000001").unwrap(),
            "second"
        );
        assert!(codex_model(&path, "00000000-0000-4000-8000-000000000002").is_err());
        transcript(
            &path,
            "00000000-0000-4000-8000-000000000001",
            "{\"type\":\"response_item\",\"payload\":{\"model\":\"not-a-model-selection\"}}\n",
        );
        assert!(codex_model(&path, "00000000-0000-4000-8000-000000000001").is_err());
        transcript(&path, "00000000-0000-4000-8000-000000000001", "{\"type\":\"turn_context\",\"payload\":{\"model\":\"first\"}}\n{\"type\":\"turn_context\",\"payload\":{\"model\":null}}\n");
        assert!(codex_model(&path, "00000000-0000-4000-8000-000000000001").is_err());
    }

    #[test]
    fn synchronization_updates_only_model_and_preserves_identity_and_launch_context() {
        let core = crate::test_support::test_core();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout.jsonl");
        let key = uuid::Uuid::new_v4().to_string();
        let mut row = crate::test_support::test_session_row(
            "native-model",
            crate::model::SessionStatus::Stopped,
        );
        row.agent_session_key = Some(key.clone());
        row.agent_runtime = Some("codex".into());
        let mut role = crate::session::manager::runtime_direct_role(
            "codex",
            Some("mock-codex"),
            Some("original-model"),
            None,
        )
        .unwrap();
        role.env.insert(
            "CODEX_HOME".into(),
            root.path()
                .join("codex-home")
                .to_string_lossy()
                .into_owned(),
        );
        row.launch_definition = Some(crate::session::launch_definition::from_effective(&role));
        let before = row.launch_definition.clone().unwrap();
        crate::test_support::insert_session_row(&core.db.get().unwrap(), &row).unwrap();
        transcript(
            &path,
            &key,
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"changed-native-model\"}}\n",
        );
        let bytes_before = std::fs::read(&path).unwrap();
        session_sync_model(&core, &row.id, &path).unwrap();
        let after = repo::session::get_row(&core.db.get().unwrap(), &row.id)
            .unwrap()
            .unwrap();
        let mut expected = before;
        expected.model = Some("changed-native-model".into());
        assert_eq!(after.launch_definition.unwrap(), expected);
        assert_eq!(after.agent_session_key, Some(key));
        assert_eq!(after.status, row.status);
        assert_eq!(after.cwd, row.cwd);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes_before,
            "native transcript must remain untouched"
        );
    }

    #[test]
    fn large_transcript_tail_and_unsupported_runtime_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout.jsonl");
        let padding = "x".repeat(MAX_TAIL_BYTES as usize + 100);
        transcript(
            &path,
            "00000000-0000-4000-8000-000000000001",
            &format!(
                "{}\n{}\n",
                json!({"type":"response_item","payload":{"text":padding}}),
                json!({"type":"turn_context","payload":{"model":"tail-model"}})
            ),
        );
        assert_eq!(
            codex_model(&path, "00000000-0000-4000-8000-000000000001").unwrap(),
            "tail-model"
        );
        let core = crate::test_support::test_core();
        let mut row = crate::test_support::test_session_row(
            "unsupported",
            crate::model::SessionStatus::Stopped,
        );
        row.agent_runtime = Some("antigravity".into());
        crate::test_support::insert_session_row(&core.db.get().unwrap(), &row).unwrap();
        assert!(session_sync_model(&core, &row.id, &path).is_err());
    }
}
