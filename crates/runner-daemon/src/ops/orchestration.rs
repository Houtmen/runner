use crate::{
    error::{Error, Result},
    AppCore,
};
use runner_core::protocol::{OrchestrationSession, OrchestrationSnapshot, SessionLimitInfo};
use rusqlite::OptionalExtension;

const LIMIT_KEY: &str = "orchestration.session_limit";

pub(crate) fn saved_session_limit(db: &crate::db::DbPool) -> Result<Option<Option<usize>>> {
    let value: Option<String> = db
        .get()?
        .query_row(
            "SELECT value FROM _app_state WHERE key=?1",
            [LIMIT_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let maximum = value
        .map(|text| serde_json::from_str::<Option<usize>>(&text))
        .transpose()?;
    if maximum == Some(Some(0)) {
        return Err(Error::msg("saved session limit must be positive"));
    }
    Ok(maximum)
}

pub fn session_limit_set(core: &AppCore, maximum: Option<usize>) -> Result<SessionLimitInfo> {
    core.sessions.save_session_limit(maximum, || {
        core.db.get()?.execute(
            "INSERT INTO _app_state(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![LIMIT_KEY, serde_json::to_string(&maximum)?]
        )?;
        Ok(())
    })?;
    let info = core.sessions.session_limit_info();
    core.events.emit("orchestration/changed", &info);
    Ok(info)
}

pub fn orchestration_snapshot(core: &AppCore) -> Result<OrchestrationSnapshot> {
    let conn = core.db.get()?;
    let ids = conn.prepare("SELECT id FROM sessions WHERE archived_at IS NULL AND agent_runtime IS NOT NULL ORDER BY started_at DESC, id")?
        .query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut sessions = Vec::new();
    for id in ids {
        if let Some(row) = crate::repo::session::get_row(&conn, &id)? {
            sessions.push(OrchestrationSession {
                title: row.title.clone().unwrap_or_else(|| row.id.clone()),
                id: row.id,
                runtime: row.agent_runtime,
                status: row.status,
                role_id: row.role_id,
            });
        }
    }
    drop(conn);
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))
        .ok_or_else(|| Error::msg("cannot resolve Codex home"))?;
    Ok(OrchestrationSnapshot {
        limit: core.sessions.session_limit_info(),
        codex_home: codex_home.to_string_lossy().into_owned(),
        roles: crate::ops::role::role_list(core)?,
        sessions,
        missions: crate::ops::mission::mission_list(core, None)?,
    })
}

pub fn role_codex_home_set(
    core: &AppCore,
    role_id: &str,
    home: Option<String>,
) -> Result<crate::model::Role> {
    let home = home
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if home
        .as_ref()
        .is_some_and(|value| !std::path::Path::new(value).is_absolute())
    {
        return Err(Error::msg("Codex home must be an absolute folder path"));
    }
    let mut conn = core.db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let role = crate::ops::role::get(&tx, role_id)?;
    if role.runtime != "codex" {
        return Err(Error::msg("Codex home is only available for Codex roles"));
    }
    let mut env = role.env;
    match home {
        Some(value) => {
            env.insert("CODEX_HOME".into(), value);
        }
        None => {
            env.remove("CODEX_HOME");
        }
    }
    tx.execute(
        "UPDATE roles SET env_json=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![
            role_id,
            serde_json::to_string(&env)?,
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    tx.commit()?;
    drop(conn);
    core.events.emit("role/changed", &());
    crate::ops::role::role_get(core, role_id)
}

pub fn mission_review_get(
    core: &AppCore,
    mission_id: &str,
) -> Result<runner_core::event_log::review::ReviewState> {
    let mission = crate::ops::mission::mission_get(core, mission_id)?;
    let path =
        runner_core::event_log::mission_dir(&core.app_data_dir, &mission.crew_id, mission_id);
    let log = runner_core::event_log::EventLog::open(&path)?;
    let entries = log.read_from(0)?;
    runner_core::event_log::review::ReviewState::from_events(
        entries.iter().map(|entry| &entry.event),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_limit_survives_boot_and_invalid_save_preserves_state() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = crate::daemon::boot::NativePaths::for_home(root.path(), true);
        paths.app_data_dir = root.path().join("runner-data");
        paths.log_dir = root.path().join("logs");
        let core = crate::daemon::boot::boot(&paths, Vec::new(), false).unwrap();
        assert_eq!(session_limit_set(&core, Some(2)).unwrap().maximum, Some(2));
        assert!(session_limit_set(&core, Some(0)).is_err());
        assert_eq!(core.sessions.session_limit_info().maximum, Some(2));
        assert_eq!(saved_session_limit(&core.db).unwrap(), Some(Some(2)));
        core.db.get().unwrap().execute_batch("CREATE TRIGGER fail_limit_save BEFORE UPDATE ON _app_state WHEN NEW.key='orchestration.session_limit' BEGIN SELECT RAISE(ABORT,'disk failure'); END;").unwrap();
        assert!(session_limit_set(&core, Some(3)).is_err());
        assert_eq!(core.sessions.session_limit_info().maximum, Some(2));
        assert_eq!(saved_session_limit(&core.db).unwrap(), Some(Some(2)));
        core.db
            .get()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_limit_save;")
            .unwrap();
        let reopened = crate::daemon::boot::boot(&paths, Vec::new(), false).unwrap();
        assert_eq!(reopened.sessions.session_limit_info().maximum, Some(2));
        session_limit_set(&reopened, None).unwrap();
        let reopened = crate::daemon::boot::boot(&paths, Vec::new(), false).unwrap();
        assert_eq!(reopened.sessions.session_limit_info().maximum, None);
    }

    #[test]
    fn home_override_preserves_other_environment_and_rejects_wrong_runtime() {
        let core = crate::test_support::test_core();
        let conn = core.db.get().unwrap();
        crate::test_support::insert_test_role(&conn, "codex-test", "codex-test", "codex", "codex");
        crate::test_support::insert_test_role(
            &conn,
            "claude-test",
            "claude-test",
            "claude-code",
            "claude",
        );
        conn.execute(
            "UPDATE roles SET env_json='{\"OTHER\":\"keep\"}' WHERE id='codex-test'",
            [],
        )
        .unwrap();
        drop(conn);
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("isolated").to_string_lossy().into_owned();
        let role = role_codex_home_set(&core, "codex-test", Some(home.clone())).unwrap();
        assert_eq!(role.env["OTHER"], "keep");
        assert_eq!(role.env["CODEX_HOME"], home);
        assert!(role_codex_home_set(&core, "codex-test", Some("relative".into())).is_err());
        assert!(role_codex_home_set(&core, "claude-test", Some(home)).is_err());
        let role = role_codex_home_set(&core, "codex-test", None).unwrap();
        assert_eq!(role.env["OTHER"], "keep");
        assert!(!role.env.contains_key("CODEX_HOME"));
    }
    #[test]
    fn snapshot_excludes_archived_sessions_and_review_read_fails_closed() {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        let core = crate::test_support::test_core_in(root.path().to_owned());
        let conn = core.db.get().unwrap();
        for (id, archived) in [("visible", false), ("archived", true)] {
            let mut row = crate::repo::session::SessionRowDb::new_running(id.into());
            row.agent_runtime = Some("codex".into());
            if archived {
                row.archived_at = Some(chrono::Utc::now());
            }
            crate::repo::session::insert(&conn, &row).unwrap();
        }
        conn.execute_batch("INSERT INTO crews(id,name,created_at,updated_at) VALUES('c','Crew','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'); INSERT INTO missions(id,crew_id,title,status,started_at) VALUES('m','c','Mission','running','2026-01-01T00:00:00Z');").unwrap();
        drop(conn);
        let snapshot = orchestration_snapshot(&core).unwrap();
        assert_eq!(
            snapshot
                .sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["visible"]
        );
        let dir = runner_core::event_log::mission_dir(root.path(), "c", "m");
        let log = runner_core::event_log::EventLog::open(&dir).unwrap();
        log.append(runner_core::model::EventDraft::signal(
            "c",
            "m",
            "human",
            runner_core::model::SignalType("review_policy".into()),
            serde_json::json!({"max_repairs":2}),
        ))
        .unwrap();
        assert_eq!(mission_review_get(&core, "m").unwrap().max_repairs, Some(2));
        std::fs::OpenOptions::new()
            .append(true)
            .open(log.path())
            .unwrap()
            .write_all(b"invalid review event\n")
            .unwrap();
        assert!(mission_review_get(&core, "m").is_err());
    }
}
