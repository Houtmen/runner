# 701 — desktop notifications: plan

Implementation plan for [#701](https://github.com/yicheng47/runner/issues/701) (feature, P2, milestone 0.16). The spec is [`docs/features/701-desktop-notifications.md`](../features/701-desktop-notifications.md) and the design is [`design/specs/701-desktop-notifications.pen`](../../design/specs/701-desktop-notifications.pen): the Settings → Notifications pane `K8FVAY` and the notification content `w9LiW` (dark) and `Ozj1d` (light). Where this plan and the spec disagree, the spec wins. Line numbers are as of `8c47a317`; paths are under `crates/runner-app/src/` unless stated.

The work is app-side only: `runnerd`, the client protocol and the database do not change. It lands as one pull request from one mission, in three commits that each build and pass their tests: the setting and its pane, direct chats, then missions.

## What exists today

- **The notification API.** `gpui-pre` 0.3.7 has `App::set_app_identity`, `show_system_notification`, `dismiss_system_notification` and `on_system_notification_response` (`gpui-pre/src/app.rs` 1628–1663). A `SystemNotification` is `{ tag, title, body, actions }` (`platform.rs` 441). The test platform records what was shown and dismissed and can simulate a click: `TestAppContext::shown_system_notifications`, `dismissed_system_notifications` and `simulate_system_notification_response` (`app/test_context.rs` 440–455). On macOS, dismissing removes both the pending and the delivered notification (`gpui-pre-macos/src/system_notifications.rs` 233).
- **Statuses.** `AppStore::session_statuses` (`app_store.rs` 267) maps every session id to an `AgentStatus` (`runner-core/src/protocol/status.rs` 80): `lifecycle`, `exit_code`, `error_since`, `failed_since`, `unread_since`, and an `observation` with `activity`, `outcome` and `interactions`. Each `HumanInteraction` has its own `id` and a `reason` (`Approval`, `Answer`, `Unknown`). `refresh_activity_inner` (793) replaces the map and bumps `revisions.activity` on `session/status`, `window_focus_map` and `usage/updated`.
- **Unread.** `record_session_completion` (`runner-daemon/src/ops/node.rs` 525) sets `unread_since` after an armed turn, skips shells, and does not set it when a focused window's registry entry views the session.
- **Missions.** `AppStore::missions` holds a `MissionSummary` per mission (`runner-core/src/protocol/mission.rs` 70) with the mission's `title` and its slots' `session_statuses`. Slot handles and runtimes come from `client.session_list(mission_id)` as `SessionRow { handle, runtime, … }`.
- **The event stream drops payloads.** `AppStore::new` subscribes once (`app_store.rs` 373) and turns each event into a `StoreRefreshKind`, so the store never sees an appended event's content. `start_focus_map_listener` (`surfaces/windowing.rs` 60) shows the pattern for a second subscription that keeps what it needs.
- **Questions.** The router answers an `ask_human` by appending a `human_question` whose payload has `triggered_by` (the `ask_human` event id), `prompt`, `choices` and an optional `on_behalf_of` (`runner-daemon/src/router/mod.rs` 1418). The asker is the `ask_human` event's `from`. A `human_response` carries `payload.question_id`. `event/appended` carries `{ mission_id, next_offset, event }` (`runner-daemon/src/event_bus/mod.rs` 84).
- **Windows.** Each window is a `NativeRoot` with its own `tabs`, `route` and `mission_workspace`. The app finds them with `cx.windows()` and `downcast::<NativeRoot>()` (`main.rs` 1863–1895). The mission workspace's `active_tab` is `MissionTab::Feed` or `MissionTab::Session(id)` (`surfaces/mission_workspace/mod.rs` 37).
- **When a window's view changes.** `report_current_subjects` (`surfaces/windowing.rs` 19) runs on every route, tab and pane change, including from `set_route`. `sync_window_activation` (36) runs when a window gains or loses focus. Mission tab switches go through `select_mission_feed` and `select_mission_session` (`surfaces/mission_workspace/input.rs` 73 and 96).
- **Navigation.**
  - `NativeRoot::open_chat_session` (`surfaces/sidebar/activation.rs` 176) activates the tab and pane that hold a session in that window.
  - `focus_other_window` (`main.rs` 1884) raises a window and activates the app.
  - The sidebar opens a slot by writing `last_mission_terminal_ids`, then calling `NativeRoot::open_mission` and `MissionWorkspace::focus_status_session` (`surfaces/sidebar/rows.rs` 240–258). The remembered id is what a still-loading mission opens on (`surfaces/mission_workspace/attach.rs` 168).
- **Labels.** `StatusPresentation::label` (`ui/agent_status.rs` 94) gives "Approval needed", "Answer needed", "Needs you", "Response failed" and "Error". `session_label_live` (`surfaces/sidebar/elements.rs` 492) gives a chat's sidebar title; the live terminal title comes from `AppStore::bridge.session(id)`.
- **Settings.** `AppSettings` is the app's own JSON file with `#[serde(default, rename_all = "camelCase")]` (`app_settings.rs` 250), so a new field needs no migration. `AppStore::update_settings` (`app_store.rs` 658) saves it and bumps `revisions.settings`. The pane list is `SettingsPane` (`surfaces/settings_page.rs` 140), with `from_route`, its route key, `label` (191), `icon` (208), `APP_PANES` (226) and the render dispatch (1449). `surfaces/settings/agents.rs` 722–733 has a ghost button with the `external-link.svg` icon that calls `cx.open_url`.
- **Identity.** `runner_core::app_paths::app_identifier(debug)` (`runner-core/src/app_paths.rs` 19) already gives `com.wycstudios.runner` or `com.wycstudios.runner-dev`, but it is private.
- **Startup and quit.** The `application.run` closure in `main.rs` (1405) sets the globals (`GlobalAppStore` at 1460) and registers `on_app_quit` (1484).

## Decisions

1. **One controller, as an app global.** A new `notifications.rs` holds `NotificationController`, an entity kept in `GlobalNotifications`, created right after `GlobalAppStore`. It is not per window: every window has its own `NativeRoot`, so a controller on each would post once per window.
2. **Pure rules, thin entity.** The rules are pure functions over plain data, tested without GPUI: trigger detection, content, visibility of a target, and whether a posted notification is stale. The entity only feeds them from the store and windows and calls the platform API.
3. **Status triggers come from snapshots.** The controller observes the `AppStore` (`cx.observe`), so it runs after the store's update has finished and can read windows safely. It diffs `session_statuses` only when `revisions.activity` or `revisions.missions` changed. A trigger is an edge:
   - a new interaction id: Approval, Answer or Needs you, from its `reason`;
   - a direct chat's `unread_since` going from `None` to set: Finished;
   - mission slots never produce Finished; a separate mission snapshot detects `MissionSummary.activity` Busy → Idle once for the whole mission;
   - `failed_since` going from `None` to set: Response failed;
   - `lifecycle` becoming `Error`: Error, with the exit code.

   The already-loaded initial store snapshot, including an empty map, is the baseline and posts nothing. `AppStore::new` performs the initial refreshes before controller installation, so an empty map must not suppress the first new session’s edge. Snapshot only sessions whose chat or mission metadata is loaded, so an early status cannot consume a new session’s trigger before its target is known. Prefer the activity map over a mission summary for the same session, since summaries refresh asynchronously. After that, a session missing from the previous snapshot is compared with a default status, so a new chat that asks for approval at once still posts. When one refresh fires several triggers for a session, one notification posts, in the order of `StatusKind`'s priority: the wait, then Error, then Response failed, then Finished.
4. **Who a session belongs to.** A session in `session_details` is a direct chat. A session in some `MissionSummary::session_statuses` is a mission slot. Shells never post: a direct chat by its `agent_runtime`, and a slot by `SessionRow::runtime`. The controller calls `session_list(mission_id)` for a slot's handle and runtime only when that slot has a trigger to post, never on a refresh: `revisions.missions` changes on most mission events, and the call blocks the UI thread.
5. **Questions come from the controller's own subscription.** It subscribes with `client.subscribe()` like `start_focus_map_listener` and keeps only `event/appended` of type `ask_human`, `human_question` and `human_response`.
   - An `ask_human` records its `from` under its event id, in a map capped at a few hundred entries.
   - A `human_question` posts the tag `question:<event id>`, with the asker found through `triggered_by`. If the ask happened before Runner connected, the title falls back to "A crew member asked you · <mission>".
   - A `human_response` dismisses `question:<question_id>`.

   Only events that arrive live can post, so a restart never replays old questions. If the mission list has not yet refreshed for a newly started mission, hold its live question until the next relevant store refresh supplies its title; a response removes that pending question too. The event stream and the store refresh are independent, so this avoids dropping the first question.
6. **Background-only posting.** Read the windows after their leases are released. Any active Runner window suppresses new notifications on every route, regardless of target visibility. Diff and record statuses/mission activity while frontmost or switched off, so enabling a switch or backgrounding Runner never replays an old edge. Keep target visibility for withdrawing existing notifications: all panes of the focused chat tab, the open mission slot, the feed for a question, and any tab of the matching mission for mission idle.
7. **Withdrawal.** The controller keeps `posted: BTreeMap<tag, Posted { target, kind }>` and dismisses a tag when:
   - **its target comes into view.** `report_current_subjects`, `sync_window_activation`, `select_mission_feed`, `select_mission_session`, closing an active slot tab, completion of the asynchronous mission load and fallback to Feed after an asynchronous slot removal call `notifications::view_changed(cx)`. That function defers the check with `cx.defer`, because these callers hold a lease on their own window.
   - **the state that posted it no longer holds,** or the session is gone (archived or closed). The waits clear when the interactions are empty, chat Finished when `unread_since` clears, mission idle when activity leaves Idle, Response failed when `failed_since` clears, and Error when the lifecycle leaves `Error`.
   - **its event’s switch is turned off,** checked when `revisions.settings` changes.
   - **a `human_response` answers its question.**
   - **Runner quits.** The `on_app_quit` handler dismisses every posted tag first.
8. **Content.**
   - **A direct chat:** the title is `session_label_live` with the bridge's live title, and the body is "<status> · <project name>", or the status alone without a project.
   - **A slot:** the title is "@<handle> · <mission title>", and the body is the status.
   - **Mission idle:** tag `mission:<mission id>:idle`, title the mission title, body "Every crew member is idle"; no session metadata lookup.
   - **A question:** the title is "@<asker> asked you · <mission title>", and the body is the prompt; the system wraps and cuts it.
   - **Status text:** it comes from `StatusPresentation::label`, plus "Finished", and "Error · exit code N" when there is an exit code.
   - **No actions or sound:** `actions` is always empty, and nothing sets sound.
9. **Clicking.** `cx.on_system_notification_response` is registered at startup and calls `notifications::open(tag, cx)`, which looks the tag up in `posted`. A tag left over from an earlier run is looked up by session id in `session_details` and `missions`; a question tag from an earlier run only brings Runner forward.
   - **A chat:** the window currently displaying the session (most recently focused if more than one does), otherwise a window whose `tabs` hold it, otherwise the main window. Every `NativeRoot` loads the shared tab list, so looking only at `tabs` cannot identify the right window. This corrects the original routing assumption after inspecting `NativeRoot::new` during implementation. Raise it as `focus_other_window` does, then call `open_chat_session`.
   - **A slot:** write `last_mission_terminal_ids[mission] = session`, as the sidebar does, then call `open_mission` and `focus_status_session` on the window that shows the mission, or on the active window.
   - **A question or mission idle:** remove `last_mission_terminal_ids[mission]` and call `open_mission`. A new `MissionWorkspace::focus_feed(mission_id)`, the feed counterpart of `focus_status_session`, selects the feed when the mission is already loaded.
10. **The setting.** `AppSettings` gains `notifications: NotificationSettings`. It holds `chats: ChatNotifications { input, finished, failed }` with all three on, and `missions: MissionNotifications { question, input, idle, failed }` with all four off. Jason removed the mission master in the final 2026-10-13 correction. Each mission switch controls only its event; slots never notify finished turns. Existing settings without these fields load those defaults.
11. **The pane.**
    - **Placement:** `SettingsPane::Notifications`, with the route key `notifications`, the label Notifications and `bell.svg` (the lucide bell, added to `assets.rs`). It goes in `APP_PANES` after Appearance.
    - **Rendering:** it is drawn in a new `surfaces/settings/notifications.rs`, like the other panes in that folder, matching `K8FVAY`. A System section comes first, then Chats and Missions sections with the existing settings row and `Toggle`. The four mission rows are always shown in one card, with independent toggles all off by default. `K8FVAY` and `gYgga` show the final dark and light pane; main includes the design in `ce2b8f81`.
    - **The System row:** it reports on or off, with a generic subtitle until the system answers. Its bordered secondary button with `external-link.svg` always opens `x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=<running bundle id>` on macOS. Not-determined and denied both show off; there is no Allow button or separate not-set-up state (Jason, 2026-10-14). `notifications::install` requests authorization at launch on a bundled macOS build, so Runner is listed in System Settings before its first notification; the system prompts only while the answer is undecided. On Windows the button reads Open Settings and opens `ms-settings:notifications`, and the subtitle names Settings → System → Notifications.
12. **Identity.** `app_identifier` becomes `pub`. The `run` closure calls `cx.set_app_identity(&app_identifier(cfg!(debug_assertions)), "Runner")` before any window opens. On Windows this registers the AppUserModelID, which matches the installer shortcut (`script/windows/runner.iss` 71); on macOS the bundle's identifier is used.

## Where it lands

- **`runner-core/src/app_paths.rs`**: `app_identifier` made `pub`.
- **`app_settings.rs`**: `NotificationSettings` and the two section structs, the `notifications` field and its default, and a round-trip test that an old settings file without the field loads the defaults.
- **`assets.rs`**: `bell.svg`.
- **`surfaces/settings_page.rs` and the new `surfaces/settings/notifications.rs`**: the pane described in decision 11.
- **`notifications.rs`** (new, with its tests in a `tests` module, or in `notifications/tests.rs` if the module grows large):
  - the pure functions `detect_status_triggers`, `notification_content`, `detect_mission_idle`, `target_visible` and `stale_tags`;
  - `NotificationController`, `GlobalNotifications`, `install(cx)`, `view_changed(cx)`, `open(tag, cx)` and `dismiss_all(cx)`.
- **`main.rs`**: `mod notifications`, the identity call, `notifications::install(cx)` after `GlobalAppStore`, the response handler, and `dismiss_all` at the top of `on_app_quit`.
- **`surfaces/windowing.rs`**: `view_changed` in `report_current_subjects` and `sync_window_activation`.
- **`surfaces/mission_workspace/events.rs`**: defer a view check when an asynchronous refresh exposes Feed after a slot disappears or the mission is archived.
- **`surfaces/mission_workspace/attach.rs`**: defer a view check once loading reveals the feed or remembered slot; the route-change hook runs before that content is visible.
- **`surfaces/mission_workspace/input.rs`**: `view_changed` in `select_mission_feed` and `select_mission_session`, and `focus_feed`.
- **`docs/tests/701-desktop-notifications.md`**: the QA record.

## Commits

1. **`feat(ui): add the Notifications settings pane`**: the settings type, the pane, the bell icon and the identity call. Nothing posts yet; the switches are saved.
2. **`feat(ui): notify when a direct chat needs you`**:
   - the controller, the status triggers for direct chats, content, visibility, withdrawal, click routing and quit;
   - the unit and controller tests for those.
3. **`feat(mission): notify when a mission needs you`**: slot triggers, whole-mission idle, the question subscription, `focus_feed`, mission visibility and routing, and their tests.

## Verification

- **Unit tests on the pure functions:**
  - every trigger edge posts once and a repeat refresh posts nothing;
  - the baseline posts nothing, and a new session after the baseline does post;
  - shells never post, and slots never post Finished, and the mission Busy → Idle edge posts once only with Goes idle on;
  - one notification per session per refresh, by priority;
  - each switch, and the defaults;
  - every row of the content table, including the fallback asker and "Error · exit code N";
  - foreground suppression across tabs, routes, panes and windows; background edge posting without replay; target visibility for withdrawal;
  - `stale_tags` for each kind.
- **Controller tests with `TestAppContext`:**
  - seed the store with `test_lifecycle_store` (`app_store.rs` 1271);
  - drive status snapshots and assert on `shown_system_notifications` and `dismissed_system_notifications`;
  - call `simulate_system_notification_response` and assert the window's route, tab and focused pane;
  - check that turning a switch off and quitting dismiss what was posted.
  - after rebasing onto #889, add a joining slot to the fixture’s persistent roster and session list; verify its current handle is fetched when it posts and its missing/busy status prevents mission idle until it reports idle. Preserve the mission view-change hooks and #889 membership refresh behavior.
- **Checks**: `cargo test -p runner-app`, workspace clippy with `-D warnings`, and `cargo fmt --check`. CI must be green on macOS and Windows.
- **Live QA status:** Jason stopped the pass on 2026-10-13 and will test the PR build himself. Preserve the partial QA record and mark remaining cases Not run; do not launch further live QA. The original procedure used the development smoke-test `.app` wrapper:
  - Never use `script/bundle-mac --dev`.
  - Use the isolated development wrapper and data while this mission runs in the installed app; never launch a second production Runner or run a binary while guessing its path.
  - Record whether the unsigned wrapper needed the ad-hoc `codesign`, and whether the first notification shows or is lost while the permission prompt is up.
- **Live QA on Windows:** `.\make.cmd run`. Toasts should carry the development identity's name.
- **Manual checks**: the spec's list, recorded in `docs/tests/701-desktop-notifications.md` with evidence.

## Risks

- **The permission race.** `gpui-pre-macos` requests authorization and adds the first request without waiting, so the very first notification may not show. If QA sees that, the fix belongs in a follow-up, not in a patch to the dependency.
- **The ad-hoc wrapper.** Its authorization on macOS is untested; the spec's fallback is `codesign --force --sign -`.
- **No active window.** When Runner is in the background, `is_window_active` is false for every window, so everything counts as out of view, as the spec intends. When no window is open at all, a click falls back to `handle_reopen` (`main.rs` 1722), which reopens the main window, before routing.
