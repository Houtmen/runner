use std::rc::Rc;

use gpui::prelude::*;
use gpui::{div, AnyElement, Context, Entity, PathPromptOptions, Render, Window};
use runner_app::ui::{
    Button, ButtonSize, ButtonVariant, PaneHeader, SelectOption, SettingsCard, StyledSelect,
    TextField, Tone,
};
use runner_core::event_log::review::{ReviewState, ReviewStatus};
use runner_core::protocol::{
    DaemonClient, MissionStatus, OrchestrationSession, OrchestrationSnapshot, PostSignalInput,
    SessionStatus,
};

use crate::surfaces::settings_page::settings_section_heading;
use crate::theme;

#[derive(Clone, Copy)]
enum Picker {
    Role,
    Session,
    Mission,
    Model,
}

enum Save {
    Limit(Option<usize>),
    Home(String, Option<String>),
    Models(String, Vec<String>),
    Model(String, String),
    Stop(String),
    Sync(String, std::path::PathBuf),
    Review(String, u64),
}

fn parse_models(text: &str) -> Vec<String> {
    let mut models = Vec::new();
    for name in text
        .split([',', '\n'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if !models.iter().any(|previous| previous == name) {
            models.push(name.to_owned());
        }
    }
    models
}

fn parse_limit(text: &str) -> Result<Option<usize>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    text.trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .map(Some)
        .ok_or_else(|| "Enter a positive session limit, or leave it blank for unlimited.".into())
}

fn can_choose(session: Option<&OrchestrationSession>, alternatives: &[String]) -> bool {
    session.is_some_and(|session| {
        session.status != SessionStatus::Running && session.role_id.is_some()
    }) && !alternatives.is_empty()
}

fn text_field(
    cx: &mut Context<AgentControlsPane>,
    placeholder: &str,
    multiline: bool,
) -> Entity<TextField> {
    cx.new(|cx| {
        let field = if multiline {
            TextField::textarea(cx.focus_handle(), "", placeholder.to_owned(), 3, true)
        } else {
            TextField::new(cx.focus_handle(), "", placeholder.to_owned(), true)
        };
        field.text_size(theme::text_body())
    })
}

fn picker(
    cx: &mut Context<AgentControlsPane>,
    kind: Picker,
    id: &'static str,
) -> Entity<StyledSelect> {
    let pane = cx.weak_entity();
    cx.new(|cx| {
        StyledSelect::new(
            id,
            cx.focus_handle(),
            "",
            Vec::new(),
            Rc::new(move |value, window, cx| {
                let pane = pane.clone();
                window.defer(cx, move |_, cx| {
                    let _ = pane.update(cx, |this, cx| {
                        if this.busy || this.loading {
                            return;
                        }
                        match kind {
                            Picker::Role => this.select_role(value, cx),
                            Picker::Session => this.select_session(value, cx),
                            Picker::Mission => this.select_mission(value, cx),
                            Picker::Model => {}
                        }
                    });
                });
            }),
            cx,
        )
        .full_width(true)
    })
}

pub(crate) struct AgentControlsPane {
    client: DaemonClient,
    snapshot: Option<OrchestrationSnapshot>,
    role: Entity<StyledSelect>,
    session: Entity<StyledSelect>,
    mission: Entity<StyledSelect>,
    model: Entity<StyledSelect>,
    limit: Entity<TextField>,
    home: Entity<TextField>,
    models: Entity<TextField>,
    budget: Entity<TextField>,
    alternatives: Vec<String>,
    current_model: Option<String>,
    review: Option<ReviewState>,
    role_loading: bool,
    session_loading: bool,
    mission_loading: bool,
    loading: bool,
    busy: bool,
    generation: u64,
    role_generation: u64,
    session_generation: u64,
    mission_generation: u64,
    notice: Option<(String, Tone)>,
}

impl AgentControlsPane {
    pub(crate) fn new(client: DaemonClient, cx: &mut Context<Self>) -> Self {
        Self {
            client,
            snapshot: None,
            role: picker(cx, Picker::Role, "controls-role"),
            session: picker(cx, Picker::Session, "controls-session"),
            mission: picker(cx, Picker::Mission, "controls-mission"),
            model: picker(cx, Picker::Model, "controls-model"),
            limit: text_field(cx, "Unlimited", false),
            home: text_field(cx, "Inherit Runner's Codex home", false),
            models: text_field(cx, "One model per line; primary first", true),
            budget: text_field(cx, "Maximum repairs, including 0", false),
            alternatives: Vec::new(),
            current_model: None,
            review: None,
            role_loading: false,
            session_loading: false,
            mission_loading: false,
            loading: false,
            busy: false,
            generation: 0,
            role_generation: 0,
            session_generation: 0,
            mission_generation: 0,
            notice: None,
        }
    }

    fn disable_editors(&self, cx: &mut Context<Self>) {
        let disabled = self.busy || self.loading;
        for select in [&self.role, &self.session, &self.mission, &self.model] {
            select.update(cx, |select, cx| select.set_disabled(disabled, cx));
        }
        let role = self
            .snapshot
            .as_ref()
            .and_then(|s| s.roles.iter().find(|r| r.id == self.role.read(cx).value()));
        let role_disabled = disabled || self.role_loading || role.is_none();
        let home_disabled = role_disabled || !role.is_some_and(|r| r.runtime == "codex");
        let mission = self.snapshot.as_ref().and_then(|s| {
            s.missions
                .iter()
                .find(|m| m.id == self.mission.read(cx).value())
        });
        let budget_disabled = disabled
            || self.mission_loading
            || !mission.is_some_and(|m| m.status == MissionStatus::Running)
            || !self
                .review
                .as_ref()
                .is_some_and(|r| r.max_repairs.is_none() && r.status == ReviewStatus::Open);
        for (input, disabled) in [
            (&self.limit, disabled),
            (&self.home, home_disabled),
            (&self.models, role_disabled),
            (&self.budget, budget_disabled),
        ] {
            input.update(cx, |input, cx| input.set_disabled(disabled, cx));
        }
        let model_disabled = disabled
            || self.session_loading
            || !can_choose(self.selected_session(cx), &self.alternatives);
        self.model
            .update(cx, |select, cx| select.set_disabled(model_disabled, cx));
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        self.loading = true;
        self.generation += 1;
        let generation = self.generation;
        self.disable_editors(cx);
        let client = self.client.clone();
        let job = cx.background_spawn(async move { client.orchestration_snapshot() });
        cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(snapshot) => {
                        this.limit.update(cx, |field, cx| {
                            field.reset(
                                snapshot
                                    .limit
                                    .maximum
                                    .map(|n| n.to_string())
                                    .unwrap_or_default(),
                                cx,
                            )
                        });
                        let update = |select: &Entity<StyledSelect>,
                                      options: Vec<SelectOption>,
                                      cx: &mut Context<Self>| {
                            let previous = select.read(cx).value().to_owned();
                            let next = if options.iter().any(|option| option.value == previous) {
                                previous
                            } else {
                                options
                                    .first()
                                    .map(|option| option.value.clone())
                                    .unwrap_or_default()
                            };
                            select.update(cx, |select, cx| {
                                select.set_options(options, cx);
                                select.set_value(next.clone(), cx);
                            });
                            next
                        };
                        let role = update(
                            &this.role,
                            snapshot
                                .roles
                                .iter()
                                .map(|r| {
                                    SelectOption::new(
                                        &r.id,
                                        format!("@{} · {}", r.handle, r.runtime),
                                    )
                                })
                                .collect(),
                            cx,
                        );
                        let session = update(
                            &this.session,
                            snapshot
                                .sessions
                                .iter()
                                .map(|s| {
                                    SelectOption::new(
                                        &s.id,
                                        format!(
                                            "{} · {}",
                                            s.title,
                                            s.id.chars().take(8).collect::<String>()
                                        ),
                                    )
                                })
                                .collect(),
                            cx,
                        );
                        let mission = update(
                            &this.mission,
                            snapshot
                                .missions
                                .iter()
                                .map(|m| SelectOption::new(&m.id, m.title.clone()))
                                .collect(),
                            cx,
                        );
                        this.snapshot = Some(snapshot);
                        this.select_role(role, cx);
                        this.select_session(session, cx);
                        this.select_mission(mission, cx);
                    }
                    Err(error) => {
                        this.snapshot = None;
                        this.notice = Some((
                            format!("Couldn't load agent controls: {error}"),
                            Tone::Danger,
                        ));
                    }
                }
                this.disable_editors(cx);
                this.disable_editors(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn select_role(&mut self, id: String, cx: &mut Context<Self>) {
        self.role_generation += 1;
        let generation = self.role_generation;
        self.role
            .update(cx, |select, cx| select.set_value(id.clone(), cx));
        let home = self
            .snapshot
            .as_ref()
            .and_then(|s| s.roles.iter().find(|r| r.id == id))
            .and_then(|role| role.env.get("CODEX_HOME"))
            .cloned()
            .unwrap_or_default();
        self.home.update(cx, |field, cx| field.reset(home, cx));
        self.models.update(cx, |field, cx| field.reset("", cx));
        self.role_loading = !id.is_empty();
        self.disable_editors(cx);
        if id.is_empty() {
            cx.notify();
            return;
        }
        let client = self.client.clone();
        let job = cx.background_spawn(async move { client.role_models_get(&id) });
        cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |this, cx| {
                if this.role_generation != generation {
                    return;
                }
                this.role_loading = false;
                match result {
                    Ok(value) => {
                        let models = value["models"]
                            .as_array()
                            .map(|models| {
                                models
                                    .iter()
                                    .filter_map(|m| m.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        this.models.update(cx, |field, cx| field.reset(models, cx));
                    }
                    Err(error) => {
                        this.role_loading = true;
                        this.notice = Some((error.to_string(), Tone::Danger));
                    }
                }
                this.disable_editors(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn select_session(&mut self, id: String, cx: &mut Context<Self>) {
        self.session_generation += 1;
        let generation = self.session_generation;
        self.session
            .update(cx, |select, cx| select.set_value(id.clone(), cx));
        self.alternatives.clear();
        self.current_model = None;
        self.model.update(cx, |select, cx| {
            select.set_options(Vec::new(), cx);
            select.set_value("", cx);
        });
        self.session_loading = !id.is_empty();
        self.disable_editors(cx);
        if id.is_empty() {
            cx.notify();
            return;
        }
        let client = self.client.clone();
        let job = cx.background_spawn(async move { client.session_model_choices(&id) });
        cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |this, cx| {
                if this.session_generation != generation {
                    return;
                }
                this.session_loading = false;
                match result {
                    Ok(value) => {
                        this.current_model = value["current_model"].as_str().map(str::to_owned);
                        this.alternatives = value["alternatives"]
                            .as_array()
                            .map(|models| {
                                models
                                    .iter()
                                    .filter_map(|m| m.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        this.model.update(cx, |select, cx| {
                            select.set_options(
                                this.alternatives
                                    .iter()
                                    .map(|m| SelectOption::new(m.clone(), m.clone()))
                                    .collect(),
                                cx,
                            );
                            select.set_value(
                                this.alternatives.first().cloned().unwrap_or_default(),
                                cx,
                            );
                        });
                    }
                    Err(error) => {
                        this.session_loading = true;
                        this.notice = Some((error.to_string(), Tone::Danger));
                    }
                }
                this.disable_editors(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn select_mission(&mut self, id: String, cx: &mut Context<Self>) {
        self.mission_generation += 1;
        let generation = self.mission_generation;
        self.mission
            .update(cx, |select, cx| select.set_value(id.clone(), cx));
        self.review = None;
        self.budget.update(cx, |field, cx| field.reset("", cx));
        self.mission_loading = !id.is_empty();
        self.disable_editors(cx);
        if id.is_empty() {
            cx.notify();
            return;
        }
        let client = self.client.clone();
        let job = cx.background_spawn(async move { client.mission_review_get(&id) });
        cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |this, cx| {
                if this.mission_generation != generation {
                    return;
                }
                this.mission_loading = false;
                match result {
                    Ok(review) => {
                        this.budget.update(cx, |field, cx| {
                            field.reset(
                                review
                                    .max_repairs
                                    .map(|n| n.to_string())
                                    .unwrap_or_default(),
                                cx,
                            )
                        });
                        this.review = Some(review);
                    }
                    Err(error) => {
                        this.mission_loading = true;
                        this.notice = Some((error.to_string(), Tone::Danger));
                    }
                }
                this.disable_editors(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn selected_session<'a>(&'a self, cx: &gpui::App) -> Option<&'a OrchestrationSession> {
        let id = self.session.read(cx).value();
        self.snapshot
            .as_ref()?
            .sessions
            .iter()
            .find(|session| session.id == id)
    }

    fn save(&mut self, action: Save, cx: &mut Context<Self>) {
        if self.busy || self.loading || self.snapshot.is_none() {
            return;
        }
        self.busy = true;
        self.notice = None;
        // Invalidate pending detail reads before any write; they must not replace
        // freshly saved values or drafts when they complete out of order.
        self.role_generation += 1;
        self.session_generation += 1;
        self.mission_generation += 1;
        self.disable_editors(cx);
        let success = match &action {
            Save::Limit(_) => "Session limit saved.",
            Save::Home(_, _) => "Codex home saved for new sessions.",
            Save::Models(_, _) => "Model preferences saved.",
            Save::Model(_, _) => "Model selected. Resume from the chat when ready.",
            Save::Stop(_) => "Session stopped.",
            Save::Sync(_, _) => "Saved Codex model synchronized.",
            Save::Review(_, _) => "Review budget saved for this mission.",
        };
        let client = self.client.clone();
        let job = cx.background_spawn(async move {
            match action {
                Save::Limit(maximum) => client.session_limit_set(maximum).map(|_| ()),
                Save::Home(id, home) => client.role_codex_home_set(&id, home).map(|_| ()),
                Save::Models(id, models) => client.role_models_set(&id, models).map(|_| ()),
                Save::Model(id, model) => client.session_choose_model(&id, &model).map(|_| ()),
                Save::Stop(id) => client.session_kill(&id).map(|_| ()),
                Save::Sync(id, transcript) => {
                    client.session_sync_model(&id, transcript).map(|_| ())
                }
                Save::Review(id, maximum) => client
                    .mission_signal_impl(PostSignalInput {
                        mission_id: id,
                        from: None,
                        signal_type: "review_policy".into(),
                        payload: serde_json::json!({"max_repairs":maximum}),
                    })
                    .map(|_| ()),
            }
        });
        cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                this.notice = Some(match result {
                    Ok(()) => (success.into(), Tone::Info),
                    Err(error) => (format!("Couldn't apply change: {error}"), Tone::Danger),
                });
                this.disable_editors(cx);
                this.refresh(cx);
                this.disable_editors(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save_limit(&mut self, cx: &mut Context<Self>) {
        match parse_limit(self.limit.read(cx).text()) {
            Ok(maximum) => self.save(Save::Limit(maximum), cx),
            Err(error) => {
                self.notice = Some((error, Tone::Danger));
                cx.notify();
            }
        }
    }

    fn save_budget(&mut self, cx: &mut Context<Self>) {
        match self.budget.read(cx).text().trim().parse::<u64>() {
            Ok(maximum) => self.save(
                Save::Review(self.mission.read(cx).value().to_owned(), maximum),
                cx,
            ),
            Err(_) => {
                self.notice = Some((
                    "Enter a whole number of repairs, including 0.".into(),
                    Tone::Danger,
                ));
                cx.notify();
            }
        }
    }

    fn browse(&mut self, transcript: bool, cx: &mut Context<Self>) {
        let generation = if transcript {
            self.session_generation
        } else {
            self.role_generation
        };
        let id = if transcript {
            self.session.read(cx).value()
        } else {
            self.role.read(cx).value()
        }
        .to_owned();
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: transcript,
            directories: !transcript,
            multiple: false,
            prompt: Some(
                if transcript {
                    "Choose this Codex session's rollout"
                } else {
                    "Choose a separate Codex home"
                }
                .into(),
            ),
        });
        cx.spawn(async move |weak, cx| {
            let selected = selected
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            let _ = weak.update(cx, |this, cx| {
                if this.busy || this.loading {
                    return;
                }
                if transcript {
                    if this.session_generation != generation || this.session.read(cx).value() != id
                    {
                        return;
                    }
                    if let Some(path) = selected {
                        this.save(Save::Sync(id, path), cx);
                    }
                } else {
                    if this.role_generation != generation || this.role.read(cx).value() != id {
                        return;
                    }
                    if let Some(path) = selected {
                        this.home.update(cx, |field, cx| {
                            field.reset(path.to_string_lossy().into_owned(), cx)
                        });
                    }
                }
            });
        })
        .detach();
    }
}

fn field(label: &'static str, control: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .child(
            div()
                .text_size(theme::text_body())
                .text_color(theme::text())
                .child(label),
        )
        .child(control)
        .into_any_element()
}

fn note(text: impl Into<gpui::SharedString>) -> AnyElement {
    div()
        .text_size(theme::text_meta())
        .text_color(theme::muted())
        .child(text.into())
        .into_any_element()
}

fn card(label: &'static str, children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(settings_section_heading(label))
        .child(SettingsCard::new([div()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .children(children)
            .into_any_element()]))
        .into_any_element()
}

impl Render for AgentControlsPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.busy || self.loading || self.snapshot.is_none();
        let selected_role = self
            .snapshot
            .as_ref()
            .and_then(|s| s.roles.iter().find(|r| r.id == self.role.read(cx).value()));
        let role_disabled = disabled || self.role_loading || selected_role.is_none();
        let codex_role = selected_role.is_some_and(|r| r.runtime == "codex");
        let selected_session = self.selected_session(cx);
        let session_disabled = disabled || self.session_loading || selected_session.is_none();
        let running = selected_session.is_some_and(|s| s.status == SessionStatus::Running);
        let codex_session = selected_session.is_some_and(|s| s.runtime.as_deref() == Some("codex"));
        let choose_enabled = can_choose(selected_session, &self.alternatives);
        let selected_mission = self.snapshot.as_ref().and_then(|s| {
            s.missions
                .iter()
                .find(|m| m.id == self.mission.read(cx).value())
        });
        let budget_enabled = selected_mission.is_some_and(|m| m.status == MissionStatus::Running)
            && self
                .review
                .as_ref()
                .is_some_and(|r| r.max_repairs.is_none() && r.status == ReviewStatus::Open);
        let limit_status = self
            .snapshot
            .as_ref()
            .map(|s| {
                format!(
                    "In use: {} / {}",
                    s.limit.active,
                    s.limit
                        .maximum
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "unlimited".into())
                )
            })
            .unwrap_or_else(|| "Loading…".into());
        let global_home = self
            .snapshot
            .as_ref()
            .map(|s| format!("Runner's Codex home: {}", s.codex_home))
            .unwrap_or_default();
        let session_status = format!(
            "Current model: {} · {}",
            self.current_model.as_deref().unwrap_or("agent default"),
            if running { "running" } else { "stopped" }
        );
        let review_status = self
            .review
            .as_ref()
            .map(|r| {
                format!(
                    "Repairs: {} / {} · {}",
                    r.repairs,
                    r.max_repairs
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "unlimited".into()),
                    match r.status {
                        ReviewStatus::Open => "open",
                        ReviewStatus::Complete => "complete",
                        ReviewStatus::Exhausted => "exhausted",
                    }
                )
            })
            .unwrap_or_else(|| "Select a mission to see its review budget.".into());
        let pane = cx.weak_entity();
        let refresh_pane = pane.clone();
        let limit_pane = pane.clone();
        let home_pane = pane.clone();
        let home_browse_pane = pane.clone();
        let models_pane = pane.clone();
        let stop_pane = pane.clone();
        let choose_pane = pane.clone();
        let sync_pane = pane.clone();
        let review_pane = pane;
        div().id("agent-controls-page").flex().flex_col().gap_5()
            .when(cfg!(test), |root| root.debug_selector(|| "AGENT_CONTROLS".into()))
            .child(PaneHeader::new("Agent controls", "Manage parallel sessions, model choices and review budgets.").action(
                Button::new("controls-refresh", "Refresh").size(ButtonSize::Sm).variant(ButtonVariant::Secondary).disabled(self.busy || self.loading)
                    .on_press(move |_, cx| { let _ = refresh_pane.update(cx, |this, cx| this.refresh(cx)); })))
            .children(self.notice.as_ref().map(|(text, tone)| runner_app::ui::notice_banner(text.clone(), *tone)))
            .child(card("Parallel sessions", vec![
                note(limit_status), field("Maximum sessions", self.limit.clone()),
                note("Leave blank for unlimited. Saved limits survive restart and override the launcher default. Existing sessions keep running."),
                Button::new("controls-save-limit", "Save limit").size(ButtonSize::Sm).disabled(disabled)
                    .on_press(move |_, cx| { let _ = limit_pane.update(cx, |this, cx| this.save_limit(cx)); }).into_any_element(),
            ]))
            .child(card("Role settings", vec![
                field("Role", self.role.clone()), note(global_home),
                field("Codex home for new sessions", self.home.clone()),
                note("Codex roles only. Leave blank to inherit. Sign in separately; existing sessions retain their saved home. Runner's Settings catalogs use the home shown above."),
                div().flex().flex_wrap().gap_2()
                    .child(Button::new("controls-browse-home", "Browse folder").size(ButtonSize::Sm).variant(ButtonVariant::Secondary).disabled(role_disabled || !codex_role)
                        .on_press(move |_, cx| { let _ = home_browse_pane.update(cx, |this, cx| this.browse(false, cx)); }))
                    .child(Button::new("controls-save-home", "Save home").size(ButtonSize::Sm).disabled(role_disabled || !codex_role)
                        .on_press(move |_, cx| { let _ = home_pane.update(cx, |this, cx| {
                            let id = this.role.read(cx).value().to_owned(); let home = this.home.read(cx).text().trim().to_owned();
                            this.save(Save::Home(id, (!home.is_empty()).then_some(home)), cx);
                        }); })).into_any_element(),
                field("Model preferences", self.models.clone()),
                note("Primary model first, then alternatives. One per line or comma-separated. Clear the list to retain the current single default."),
                Button::new("controls-save-models", "Save models").size(ButtonSize::Sm).disabled(role_disabled)
                    .on_press(move |_, cx| { let _ = models_pane.update(cx, |this, cx| {
                        let id = this.role.read(cx).value().to_owned(); let models = parse_models(this.models.read(cx).text());
                        this.save(Save::Models(id, models), cx);
                    }); }).into_any_element(),
            ]))
            .child(card("Session model", vec![
                field("Session", self.session.clone()), note(session_status),
                field("Alternative model", self.model.clone()),
                note("Stop first, choose an alternative, then resume from the chat. Role-backed sessions use their configured alternatives. No automatic retry or provider change."),
                div().flex().flex_wrap().gap_2()
                    .child(Button::new("controls-stop-session", "Stop session").size(ButtonSize::Sm).variant(ButtonVariant::Secondary).disabled(session_disabled || !running)
                        .on_press(move |_, cx| { let _ = stop_pane.update(cx, |this, cx| this.save(Save::Stop(this.session.read(cx).value().to_owned()), cx)); }))
                    .child(Button::new("controls-use-model", "Use model").size(ButtonSize::Sm).disabled(session_disabled || !choose_enabled)
                        .on_press(move |_, cx| { let _ = choose_pane.update(cx, |this, cx| this.save(Save::Model(this.session.read(cx).value().to_owned(), this.model.read(cx).value().to_owned()), cx)); })).into_any_element(),
                note("After changing a model inside Codex, choose its own rollout file to synchronize Runner's saved model. The conversation identity is verified."),
                Button::new("controls-sync-model", "Choose Codex transcript and sync").size(ButtonSize::Sm).variant(ButtonVariant::Secondary).disabled(session_disabled || !codex_session)
                    .on_press(move |_, cx| { let _ = sync_pane.update(cx, |this, cx| this.browse(true, cx)); }).into_any_element(),
            ]))
            .child(card("Mission review", vec![
                field("Mission", self.mission.clone()), note(review_status), field("Maximum repairs", self.budget.clone()),
                note("Set once on a running mission; 0 allows no repairs. Only explicit repair requests count. A budget stops new repair requests, not running processes."),
                Button::new("controls-set-review", "Set review budget").size(ButtonSize::Sm).disabled(disabled || self.mission_loading || !budget_enabled)
                    .on_press(move |_, cx| { let _ = review_pane.update(cx, |this, cx| this.save_budget(cx)); }).into_any_element(),
            ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inputs_distinguish_unlimited_and_zero_and_preserve_model_order() {
        assert_eq!(parse_limit(""), Ok(None));
        assert_eq!(parse_limit(" 3 "), Ok(Some(3)));
        for text in ["0", "-1", "1.5", "three"] {
            assert!(parse_limit(text).is_err());
        }
        assert_eq!(
            parse_models("primary\nfallback, primary\n "),
            vec!["primary", "fallback"]
        );
        assert!(parse_models(" , \n").is_empty());
    }
    #[test]
    fn model_choice_requires_stopped_role_backed_session_and_alternative() {
        let mut session = OrchestrationSession {
            id: "s".into(),
            title: "s".into(),
            runtime: Some("codex".into()),
            status: SessionStatus::Running,
            role_id: Some("r".into()),
        };
        let alternatives = vec!["fallback".into()];
        assert!(!can_choose(Some(&session), &alternatives));
        session.status = SessionStatus::Stopped;
        assert!(can_choose(Some(&session), &alternatives));
        assert!(!can_choose(Some(&session), &[]));
        session.role_id = None;
        assert!(!can_choose(Some(&session), &alternatives));
        assert!(!can_choose(None, &alternatives));
    }
    #[test]
    fn visual_controls_save_limit_home_and_model_choice_through_daemon() {
        use gpui::{px, size, TestAppContext, VisualTestContext};
        let _theme = crate::theme_snapshot::ThemeGuard::new();
        let root = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let store = crate::app_store::test_lifecycle_store(&mut cx, root.path());
        let core = store.update(&mut cx, |store, _| store.test_core.clone());
        core.db.get().unwrap().execute_batch(r#"INSERT INTO roles(id,handle,display_name,runtime,command,env_json,model,created_at,updated_at)
            VALUES('controls-coder','controls-coder','Coder','codex','codex','{"OTHER":"keep"}','primary','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"#).unwrap();
        runner_daemon::ops::model_selection::role_models_set(
            &core,
            "controls-coder",
            vec!["primary".into(), "fallback".into()],
        )
        .unwrap();
        let mut row =
            runner_daemon::repo::session::SessionRowDb::new_running("controls-session".into());
        row.role_id = Some("controls-coder".into());
        row.agent_runtime = Some("codex".into());
        row.status = SessionStatus::Stopped;
        row.agent_session_key = Some("same-conversation".into());
        row.launch_definition = Some(runner_daemon::model::LaunchDefinition {
            command: "codex".into(),
            args: vec!["--keep".into()],
            env: Default::default(),
            model: Some("primary".into()),
            effort: None,
            codex_speed: None,
            system_prompt: None,
        });
        runner_daemon::repo::session::insert(&core.db.get().unwrap(), &row).unwrap();
        let client = store.update(&mut cx, |store, _| store.client.clone());
        let host = cx.add_window(|_, cx| {
            let mut pane = AgentControlsPane::new(client, cx);
            pane.refresh(cx);
            pane
        });
        let mut visual = VisualTestContext::from_window(host.into(), &cx);
        visual.simulate_resize(size(px(1200.), px(1800.)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("AGENT_CONTROLS").is_some());
        host.update(&mut visual, |pane, _, cx| {
            pane.select_role("controls-coder".into(), cx);
            pane.select_session("controls-session".into(), cx);
        })
        .unwrap();
        visual.run_until_parked();
        host.read_with(&visual, |pane, cx| {
            assert!(!pane.loading && !pane.role_loading && !pane.session_loading);
            assert_eq!(pane.role.read(cx).value(), "controls-coder");
            assert_eq!(pane.alternatives, vec!["fallback"]);
        })
        .unwrap();
        host.update(&mut visual, |pane, _, cx| {
            pane.limit.update(cx, |field, cx| field.reset("2", cx));
            pane.save_limit(cx);
        })
        .unwrap();
        visual.run_until_parked();
        assert_eq!(core.sessions.session_limit_info().maximum, Some(2));
        host.update(&mut visual, |pane, _, cx| {
            pane.save(
                Save::Models(
                    "controls-coder".into(),
                    parse_models("primary\nfallback\nthird"),
                ),
                cx,
            );
        })
        .unwrap();
        visual.run_until_parked();
        host.read_with(&visual, |pane, _| {
            assert_eq!(pane.alternatives, vec!["fallback", "third"])
        })
        .unwrap();
        let home = root
            .path()
            .join("isolated-codex")
            .to_string_lossy()
            .into_owned();
        host.update(&mut visual, |pane, _, cx| {
            pane.save(Save::Home("controls-coder".into(), Some(home.clone())), cx);
        })
        .unwrap();
        visual.run_until_parked();
        let role = runner_daemon::ops::role::role_get(&core, "controls-coder").unwrap();
        assert_eq!(role.env["CODEX_HOME"], home);
        assert_eq!(role.env["OTHER"], "keep");
        host.update(&mut visual, |pane, _, cx| {
            pane.save(
                Save::Model("controls-session".into(), "fallback".into()),
                cx,
            )
        })
        .unwrap();
        visual.run_until_parked();
        let saved =
            runner_daemon::repo::session::get_row(&core.db.get().unwrap(), "controls-session")
                .unwrap()
                .unwrap();
        assert_eq!(saved.agent_session_key, row.agent_session_key);
        assert_eq!(
            saved.launch_definition.unwrap().model.as_deref(),
            Some("fallback")
        );
        host.read_with(&visual, |pane, _| assert!(!pane.busy))
            .unwrap();
    }
}
