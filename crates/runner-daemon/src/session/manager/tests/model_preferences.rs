use super::*;

#[test]
fn chosen_model_survives_stop_and_a_new_manager_without_changing_native_session() {
    let pool = pool_with_schema();
    let data = tempfile::tempdir().unwrap();
    let fake = fake_runtime();
    let manager = mgr_with_fake(None, Arc::clone(&fake));
    let mut core = crate::test_support::test_core();
    core.db = Arc::clone(&pool);
    core.sessions = Arc::clone(&manager);
    let mut configured = role("mock-codex", &[]);
    configured.runtime = "codex".into();
    configured.model = Some("first".into());
    insert_role_row(&pool.get().unwrap(), &configured);
    crate::ops::model_selection::role_models_set(
        &core,
        &configured.id,
        vec!["first".into(), "second".into()],
    )
    .unwrap();
    let id = ulid::Ulid::new().to_string();
    let key = uuid::Uuid::new_v4().to_string();
    let mut row = crate::repo::session::SessionRowDb::new_running(id.clone());
    row.role_id = Some(configured.id.clone());
    row.status = crate::model::SessionStatus::Stopped;
    row.agent_runtime = Some("codex".into());
    row.agent_session_key = Some(key.clone());
    row.launch_definition = Some(crate::session::launch_definition::from_effective(
        &configured,
    ));
    row.cwd = Some(data.path().to_string_lossy().into_owned());
    crate::test_support::insert_session_row(&pool.get().unwrap(), &row).unwrap();
    crate::ops::model_selection::session_choose_model(&core, &id, "second").unwrap();
    manager
        .resume_on_launch(&id, None, None, data.path(), Arc::clone(&pool), capture())
        .unwrap();
    let spec = fake.last_spawn_spec().unwrap();
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["resume", key.as_str()]));
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["--model", "second"]));
    manager.kill(&id).unwrap();
    let restarted_runtime = fake_runtime();
    let restarted = mgr_with_fake(None, Arc::clone(&restarted_runtime));
    restarted
        .resume_on_launch(&id, None, None, data.path(), Arc::clone(&pool), capture())
        .unwrap();
    let spec = restarted_runtime.last_spawn_spec().unwrap();
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["resume", key.as_str()]));
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["--model", "second"]));
    restarted.kill(&id).unwrap();
}
