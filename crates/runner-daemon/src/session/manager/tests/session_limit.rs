use super::*;

#[test]
fn runtime_admission_releases_on_failed_spawn_exit_and_successful_stop() {
    let fake = fake_runtime();
    let manager = mgr_with_fake(None, Arc::clone(&fake));
    manager.set_session_limit(Some(1)).unwrap();
    let spec = |id: &str| {
        let mut spec = manager.update_spawn_spec("mock".into(), vec![], None, (80, 24));
        spec.session_id = id.into();
        spec
    };
    *fake.spawn_failure_handle.lock().unwrap() = Some("fail".into());
    let mut failed = spec("failed");
    failed.env.insert("RUNNER_HANDLE".into(), "fail".into());
    assert!(manager.runtime.spawn(failed).is_err());
    let (first, _output) = manager.runtime.spawn(spec("first")).unwrap();
    assert!(manager
        .runtime
        .spawn(spec("denied"))
        .err()
        .unwrap()
        .to_string()
        .contains("global session limit"));
    fake.fail_stop_for("first");
    assert!(manager.runtime.stop(&first).is_err());
    assert!(manager.runtime.spawn(spec("still-denied")).is_err());
    fake.allow_stop_for("first");
    manager.runtime.stop(&first).unwrap();
    let (second, _output) = manager.runtime.spawn(spec("second")).unwrap();
    fake.close_spawn(1);
    assert!(!manager.runtime.status(&second).unwrap().unwrap().alive);
    let (third, _output) = manager.runtime.spawn(spec("third")).unwrap();
    manager.runtime.stop(&third).unwrap();
}

#[test]
fn chats_mission_slots_and_resume_share_one_limit() {
    let pool = pool_with_schema();
    let data = tempfile::tempdir().unwrap();
    let fake = fake_runtime();
    let manager = mgr_with_fake(None, Arc::clone(&fake));
    manager.set_session_limit(Some(1)).unwrap();
    let role = role("mock", &[]);
    let start_chat = || {
        manager.spawn_runtime_direct(
            &role,
            None,
            None,
            None,
            None,
            data.path(),
            Arc::clone(&pool),
            capture(),
        )
    };
    fake.status_response.lock().unwrap().alive = true;
    let first = start_chat().unwrap();
    let before = fake.spawn_count();
    assert!(start_chat()
        .unwrap_err()
        .to_string()
        .contains("global session limit"));
    let (mission, mission_role, slot) = single_slot_mission(&pool);
    assert!(manager
        .spawn(
            &mission,
            &mission_role,
            &slot,
            data.path(),
            data.path().join("events.jsonl"),
            Arc::clone(&pool),
            capture(),
            None
        )
        .unwrap_err()
        .to_string()
        .contains("global session limit"));
    assert_eq!(fake.spawn_count(), before);
    fake.status_response.lock().unwrap().alive = false;
    manager.kill(&first.id).unwrap();
    fake.status_response.lock().unwrap().alive = true;
    let second = start_chat().unwrap();
    assert!(manager
        .resume_on_launch(
            &first.id,
            None,
            None,
            data.path(),
            Arc::clone(&pool),
            capture()
        )
        .unwrap_err()
        .to_string()
        .contains("global session limit"));
    fake.status_response.lock().unwrap().alive = false;
    manager.kill(&second.id).unwrap();
    manager
        .resume_on_launch(
            &first.id,
            None,
            None,
            data.path(),
            Arc::clone(&pool),
            capture(),
        )
        .unwrap();
    manager.kill(&first.id).unwrap();
}

#[test]
fn headless_forks_cannot_bypass_the_global_limit() {
    let pool = pool_with_schema();
    let data = tempfile::tempdir().unwrap();
    let fake = fake_runtime();
    let manager = mgr_with_fake(None, Arc::clone(&fake));
    manager.set_session_limit(Some(1)).unwrap();
    fake.status_response.lock().unwrap().alive = true;
    let active = manager
        .spawn_runtime_direct(
            &role("mock", &[]),
            None,
            None,
            None,
            None,
            data.path(),
            Arc::clone(&pool),
            capture(),
        )
        .unwrap();
    let mut source_role = role("must-not-be-executed", &[]);
    source_role.runtime = "codex".into();
    let mut source = crate::repo::session::SessionRowDb::new_running(ulid::Ulid::new().to_string());
    source.status = crate::model::SessionStatus::Stopped;
    source.agent_runtime = Some("codex".into());
    source.agent_session_key = Some(uuid::Uuid::new_v4().to_string());
    source.launch_definition = Some(crate::session::launch_definition::from_effective(
        &source_role,
    ));
    source.cwd = Some(data.path().to_string_lossy().into_owned());
    crate::test_support::insert_session_row(&pool.get().unwrap(), &source).unwrap();
    let before: i64 = pool
        .get()
        .unwrap()
        .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let error = manager
        .spawn_fork(
            &source.id,
            None,
            None,
            None,
            data.path(),
            Arc::clone(&pool),
            capture(),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("global session limit"),
        "{error}"
    );
    assert_eq!(fake.spawn_count(), 1);
    let after: i64 = pool
        .get()
        .unwrap()
        .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, before, "denied forks must not leave a session row");
    fake.status_response.lock().unwrap().alive = false;
    manager.kill(&active.id).unwrap();
}
