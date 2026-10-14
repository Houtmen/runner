use super::*;

#[test]
fn planner_builder_reviewer_policies_share_capacity_budget_and_native_context() {
    let pool = pool_with_schema();
    let data = tempfile::tempdir().unwrap();
    let fake = fake_runtime();
    fake.status_response.lock().unwrap().alive = true;
    let manager = mgr_with_fake(None, Arc::clone(&fake));
    manager.set_session_limit(Some(3)).unwrap();
    let mut core = crate::test_support::test_core_in(data.path().to_owned());
    core.db = Arc::clone(&pool);
    core.sessions = Arc::clone(&manager);
    let roles: Vec<_> = ["mock-planner", "mock-builder", "mock-reviewer"]
        .into_iter()
        .map(|handle| {
            let mut configured = role("mock-codex", &[]);
            configured.handle = handle.into();
            configured.runtime = "codex".into();
            configured.model = Some("mock-primary".into());
            configured.env.insert(
                "CODEX_HOME".into(),
                data.path().join(handle).to_string_lossy().into_owned(),
            );
            configured
        })
        .collect();
    let (mut mission, first_slot) = seed_mission_rows(&pool, &roles[0]);
    pool.get()
        .unwrap()
        .execute(
            "UPDATE slots SET slot_handle='mock-planner' WHERE id=?1",
            [&first_slot.id],
        )
        .unwrap();
    mission.cwd = Some(data.path().to_string_lossy().into_owned());
    pool.get()
        .unwrap()
        .execute(
            "UPDATE missions SET cwd=?2 WHERE id=?1",
            [&mission.id, mission.cwd.as_ref().unwrap()],
        )
        .unwrap();
    let mut slots = vec![first_slot];
    for (index, configured) in roles.iter().enumerate().skip(1) {
        insert_role_row(&pool.get().unwrap(), configured);
        let mut slot = slot_for(configured);
        slot.crew_id = mission.crew_id.clone();
        slot.slot_handle = configured.handle.clone();
        slot.position = index as i64;
        slot.lead = false;
        crate::test_support::insert_test_slot(
            &pool.get().unwrap(),
            &slot.id,
            &slot.crew_id,
            &slot.role_id,
            &slot.slot_handle,
            slot.position,
            slot.lead,
        );
        slots.push(slot);
    }
    for configured in &roles {
        crate::ops::model_selection::role_models_set(
            &core,
            &configured.id,
            vec!["mock-primary".into(), "mock-alternative".into()],
        )
        .unwrap();
    }
    let mission_dir =
        runner_core::event_log::mission_dir(data.path(), &mission.crew_id, &mission.id);
    let log = runner_core::event_log::EventLog::open(&mission_dir).unwrap();
    let signal = |sender: &str, kind: &str, payload| runner_core::model::EventDraft {
        crew_id: mission.crew_id.clone(),
        mission_id: mission.id.clone(),
        kind: runner_core::model::EventKind::Signal,
        from: sender.into(),
        to: None,
        signal_type: Some(runner_core::model::SignalType::new(kind)),
        payload,
    };
    log.append(signal(
        "human",
        "review_policy",
        serde_json::json!({"max_repairs":1}),
    ))
    .unwrap();
    let mut sessions = Vec::new();
    for (configured, slot) in roles.iter().zip(&slots) {
        sessions.push(
            manager
                .spawn(
                    &mission,
                    configured,
                    slot,
                    data.path(),
                    log.path().to_owned(),
                    Arc::clone(&pool),
                    capture(),
                    None,
                )
                .unwrap(),
        );
        assert_eq!(
            fake.last_spawn_spec().unwrap().env.get("CODEX_HOME"),
            configured.env.get("CODEX_HOME")
        );
    }
    assert_eq!(fake.spawn_count(), 3);
    assert!(manager
        .spawn_runtime_direct(
            &role("mock", &[]),
            None,
            None,
            None,
            None,
            data.path(),
            Arc::clone(&pool),
            capture()
        )
        .is_err());
    let post = |from: &str, to: &str, text: &str| {
        crate::daemon::block_on(crate::ops::mission::mission_post_impl(
            &core,
            crate::ops::mission::PostMessageInput {
                mission_id: mission.id.clone(),
                from: Some(from.into()),
                to: Some(to.into()),
                text: text.into(),
            },
        ))
        .unwrap()
    };
    post("mock-planner", "mock-builder", "Implement the mock task");
    post(
        "mock-builder",
        "mock-reviewer",
        "The mock diff is ready for review",
    );
    let repair = signal(
        "reviewer",
        "repair_requested",
        serde_json::json!({"request_id":"review-1","target":"mock-builder","text":"Fix the mock regression"}),
    );
    let accepted = log.append(repair.clone()).unwrap();
    assert_eq!(log.append(repair).unwrap().id, accepted.id);
    fake.status_response.lock().unwrap().alive = false;
    manager.kill(&sessions[1].id).unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    pool.get()
        .unwrap()
        .execute(
            "UPDATE sessions SET agent_session_key=?2 WHERE id=?1",
            [&sessions[1].id, &key],
        )
        .unwrap();
    crate::ops::model_selection::session_choose_model(&core, &sessions[1].id, "mock-alternative")
        .unwrap();
    fake.status_response.lock().unwrap().alive = true;
    manager
        .resume_on_launch(
            &sessions[1].id,
            None,
            None,
            data.path(),
            Arc::clone(&pool),
            capture(),
        )
        .unwrap();
    let spec = fake.last_spawn_spec().unwrap();
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["resume", key.as_str()]));
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["--model", "mock-alternative"]));
    assert_eq!(
        fake.spawn_count(),
        4,
        "resume uses the released slot rather than bypassing capacity"
    );
    log.append(signal(
        "reviewer",
        "review_complete",
        serde_json::json!({"text":"No remaining mock findings"}),
    ))
    .unwrap();
    let reopened = runner_core::event_log::EventLog::open(&mission_dir).unwrap();
    let entries = reopened.read_from(0).unwrap();
    let state = runner_core::event_log::review::ReviewState::from_events(
        entries.iter().map(|entry| &entry.event),
    )
    .unwrap();
    assert_eq!(state.repairs, 1);
    assert_eq!(
        state.status,
        runner_core::event_log::review::ReviewStatus::Complete
    );
    assert!(reopened
        .append(signal(
            "reviewer",
            "repair_requested",
            serde_json::json!({"text":"Must not loop"})
        ))
        .is_err());
    fake.status_response.lock().unwrap().alive = false;
    for session in sessions {
        manager.kill(&session.id).unwrap();
    }
}
