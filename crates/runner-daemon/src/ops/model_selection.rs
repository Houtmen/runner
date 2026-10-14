use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::json;

use crate::error::{Error, Result};
use crate::{repo, AppCore};

fn models_for(conn: &rusqlite::Connection, role: &crate::model::Role) -> Result<Vec<String>> {
    let stored: Option<(String, String)> = conn
        .query_row(
            "SELECT runtime, models_json FROM role_model_preferences WHERE role_id = ?1",
            [&role.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match stored {
        Some((runtime, text)) if runtime == role.runtime => Ok(serde_json::from_str(&text)?),
        _ => Ok(role.model.iter().cloned().collect()),
    }
}

pub fn role_models_get(core: &AppCore, role_id: &str) -> Result<serde_json::Value> {
    let conn = core.db.get()?;
    let role = crate::ops::role::get(&conn, role_id)?;
    Ok(
        json!({"role_id": role.id, "runtime": role.runtime, "primary_model": role.model, "models": models_for(&conn, &role)?}),
    )
}

pub fn role_models_set(
    core: &AppCore,
    role_id: &str,
    models: Vec<String>,
) -> Result<serde_json::Value> {
    let mut normalized = Vec::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() {
            return Err(Error::msg("model preferences must not contain blank names"));
        }
        if !normalized.iter().any(|previous| previous == model) {
            normalized.push(model.to_owned());
        }
    }
    let mut conn = core.db.get()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let role = crate::ops::role::get(&tx, role_id)?;
    if normalized.is_empty() {
        tx.execute(
            "DELETE FROM role_model_preferences WHERE role_id = ?1",
            [role_id],
        )?;
    } else {
        tx.execute("INSERT INTO role_model_preferences(role_id, runtime, models_json) VALUES (?1, ?2, ?3) ON CONFLICT(role_id) DO UPDATE SET runtime=excluded.runtime, models_json=excluded.models_json",
            params![role_id, role.runtime, serde_json::to_string(&normalized)?])?;
        // Keep legacy clients and launch paths on the selected primary model.
        tx.execute(
            "UPDATE roles SET model=?2, updated_at=?3 WHERE id=?1",
            params![role_id, normalized[0], chrono::Utc::now().to_rfc3339()],
        )?;
    }
    tx.commit()?;
    drop(conn);
    core.events.emit("role/changed", &());
    role_models_get(core, role_id)
}

pub fn session_model_choices(core: &AppCore, session_id: &str) -> Result<serde_json::Value> {
    let conn = core.db.get()?;
    let row = repo::session::get_row(&conn, session_id)?
        .ok_or_else(|| Error::msg("session not found"))?;
    let current = row
        .launch_definition
        .as_ref()
        .and_then(|definition| definition.model.clone());
    let models = match row.role_id.as_deref() {
        Some(id) => {
            let role = crate::ops::role::get(&conn, id)?;
            if row.agent_runtime.as_deref() == Some(role.runtime.as_str()) {
                models_for(&conn, &role)?
            } else {
                Vec::new()
            }
        }
        None => Vec::new(),
    };
    let alternatives: Vec<_> = models
        .into_iter()
        .filter(|model| Some(model) != current.as_ref())
        .collect();
    Ok(
        json!({"session_id":session_id, "runtime":row.agent_runtime, "current_model":current, "alternatives":alternatives,
        "choice_requires_stop": row.status == crate::model::SessionStatus::Running, "automatic_fallback":false}),
    )
}

pub fn session_choose_model(
    core: &AppCore,
    session_id: &str,
    model: &str,
) -> Result<serde_json::Value> {
    let model = model.trim();
    if model.is_empty() {
        return Err(Error::msg("model must not be blank"));
    }
    let _claim = core
        .sessions
        .try_claim_resume(session_id)
        .ok_or_else(|| Error::msg("session is being resumed; retry after it stops"))?;
    let mut conn = core.db.get()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row =
        repo::session::get_row(&tx, session_id)?.ok_or_else(|| Error::msg("session not found"))?;
    if row.status == crate::model::SessionStatus::Running || row.archived_at.is_some() {
        return Err(Error::msg(
            "stop and unarchive the session before choosing a model",
        ));
    }
    let role_id = row
        .role_id
        .as_deref()
        .ok_or_else(|| Error::msg("configure model preferences on a role-backed session"))?;
    let role = crate::ops::role::get(&tx, role_id)?;
    if row.agent_runtime.as_deref() != Some(role.runtime.as_str()) {
        return Err(Error::msg(
            "model choices cannot change the session's runtime/provider",
        ));
    }
    if !models_for(&tx, &role)?
        .iter()
        .any(|candidate| candidate == model)
    {
        return Err(Error::msg(
            "model is not in this role's configured preferences",
        ));
    }
    let mut definition = row
        .launch_definition
        .ok_or_else(|| Error::msg("session has no recorded launch definition"))?;
    definition.model = Some(model.to_owned());
    repo::session::set_launch(&tx, session_id, &role.runtime, &definition)?;
    tx.commit()?;
    drop(conn);
    core.events.emit(
        "session/updated",
        &json!({"session_id": session_id, "mission_id": row.mission_id}),
    );
    session_model_choices(core, session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(core: &AppCore) -> crate::model::Role {
        let input = serde_json::from_value(json!({"handle":"chooser", "display_name":"Chooser", "runtime":"codex", "command":"codex", "model":"legacy"})).unwrap();
        crate::ops::role::create(&core.db.get().unwrap(), input).unwrap()
    }

    fn stopped_session(core: &AppCore, role: &crate::model::Role) -> String {
        let mut row = crate::test_support::test_session_row(
            "session-choice",
            crate::model::SessionStatus::Stopped,
        );
        row.role_id = Some(role.id.clone());
        row.agent_runtime = Some(role.runtime.clone());
        row.agent_session_key = Some(uuid::Uuid::new_v4().to_string());
        row.launch_definition = Some(crate::session::launch_definition::from_effective(role));
        crate::test_support::insert_session_row(&core.db.get().unwrap(), &row).unwrap();
        row.id
    }

    #[test]
    fn ordered_preferences_preserve_legacy_model_and_roundtrip_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runner.db");
        let mut core = crate::test_support::test_core();
        core.db = std::sync::Arc::new(crate::db::open_pool(&path).unwrap());
        let role = role(&core);
        assert_eq!(
            role_models_get(&core, &role.id).unwrap()["models"],
            json!(["legacy"])
        );
        let choices = role_models_set(
            &core,
            &role.id,
            vec![" first ".into(), "second".into(), "first".into()],
        )
        .unwrap();
        assert_eq!(choices["models"], json!(["first", "second"]));
        assert_eq!(
            crate::ops::role::get(&core.db.get().unwrap(), &role.id)
                .unwrap()
                .model
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            role_models_get(&core, &role.id).unwrap()["models"],
            json!(["first", "second"])
        );
        core.db = std::sync::Arc::new(crate::db::open_pool(&path).unwrap());
        assert_eq!(
            role_models_get(&core, &role.id).unwrap()["models"],
            json!(["first", "second"])
        );
        assert!(role_models_set(&core, &role.id, vec![" ".into()]).is_err());
        assert_eq!(
            role_models_set(&core, &role.id, vec![]).unwrap()["models"],
            json!(["first"])
        );
    }

    #[test]
    fn explicit_choice_preserves_native_history_and_other_launch_settings() {
        let core = crate::test_support::test_core();
        let role = role(&core);
        role_models_set(&core, &role.id, vec!["legacy".into(), "alternative".into()]).unwrap();
        let id = stopped_session(&core, &role);
        let before = repo::session::get_row(&core.db.get().unwrap(), &id)
            .unwrap()
            .unwrap();
        assert_eq!(
            session_model_choices(&core, &id).unwrap()["alternatives"],
            json!(["alternative"])
        );
        let chosen = session_choose_model(&core, &id, "alternative").unwrap();
        assert_eq!(chosen["current_model"], "alternative");
        let after = repo::session::get_row(&core.db.get().unwrap(), &id)
            .unwrap()
            .unwrap();
        assert_eq!(after.agent_session_key, before.agent_session_key);
        let mut expected = before.launch_definition.unwrap();
        expected.model = Some("alternative".into());
        assert_eq!(after.launch_definition.unwrap(), expected);
        assert_eq!(after.status, crate::model::SessionStatus::Stopped);
        assert!(session_choose_model(&core, &id, "unconfigured").is_err());
    }

    #[test]
    fn running_resume_claims_and_runtime_changes_refuse_model_mutation() {
        let core = crate::test_support::test_core();
        let role = role(&core);
        role_models_set(&core, &role.id, vec!["legacy".into(), "alternative".into()]).unwrap();
        let id = stopped_session(&core, &role);
        let claim = core.sessions.try_claim_resume(&id).unwrap();
        assert!(session_choose_model(&core, &id, "alternative").is_err());
        drop(claim);
        core.db
            .get()
            .unwrap()
            .execute("UPDATE sessions SET status='running' WHERE id=?1", [&id])
            .unwrap();
        assert!(session_choose_model(&core, &id, "alternative").is_err());
        core.db
            .get()
            .unwrap()
            .execute("UPDATE sessions SET status='stopped' WHERE id=?1", [&id])
            .unwrap();
        core.db
            .get()
            .unwrap()
            .execute(
                "UPDATE roles SET runtime='claude-code' WHERE id=?1",
                [&role.id],
            )
            .unwrap();
        assert!(session_choose_model(&core, &id, "alternative").is_err());
        assert_eq!(
            session_model_choices(&core, &id).unwrap()["alternatives"],
            json!([])
        );
    }
}
