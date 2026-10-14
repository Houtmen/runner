use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{Event, EventDraft, EventKind};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    #[default]
    Open,
    Complete,
    Exhausted,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewState {
    pub max_repairs: Option<u64>,
    pub repairs: u64,
    pub status: ReviewStatus,
}

pub(crate) enum Decision {
    Accept,
    Duplicate(Box<Event>),
    Exhausted,
}

impl ReviewState {
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Result<Self> {
        let mut state = Self::default();
        for event in events {
            if event.kind != EventKind::Signal {
                continue;
            }
            match event.signal_type.as_ref().map(|kind| kind.as_str()) {
                Some("review_policy") => state.max_repairs = Some(maximum(&event.payload)?),
                Some("repair_requested") => {
                    state.repairs = state
                        .repairs
                        .checked_add(1)
                        .ok_or_else(|| Error::msg("review counter overflow"))?
                }
                Some("review_complete") => state.status = ReviewStatus::Complete,
                Some("review_exhausted") => state.status = ReviewStatus::Exhausted,
                _ => {}
            }
        }
        Ok(state)
    }

    pub(crate) fn admit(&self, events: &[Event], draft: &mut EventDraft) -> Result<Decision> {
        match draft.signal_type.as_ref().map(|kind| kind.as_str()) {
            Some("review_policy") => {
                if !matches!(draft.from.as_str(), "human" | "system") {
                    return Err(Error::msg(
                        "review policy is reserved for the person or Runner",
                    ));
                }
                let maximum = maximum(&draft.payload)?;
                if self.status != ReviewStatus::Open
                    || self.max_repairs.is_some_and(|previous| previous != maximum)
                    || self.repairs > maximum
                {
                    return Err(Error::msg(
                        "cannot reset or change an active mission's review budget",
                    ));
                }
            }
            Some("repair_requested") => {
                if draft.payload.get("target").is_some_and(|target| {
                    !target
                        .as_str()
                        .is_some_and(|target| !target.trim().is_empty())
                }) {
                    return Err(Error::msg("review target must be a non-empty handle"));
                }
                let text = draft
                    .payload
                    .get("text")
                    .and_then(|value| value.as_str())
                    .filter(|text| !text.trim().is_empty());
                if text.is_none() {
                    return Err(Error::msg("repair_requested requires non-empty text"));
                }
                if let Some(request_id) = draft.payload.get("request_id") {
                    let id = request_id
                        .as_str()
                        .filter(|id| !id.trim().is_empty())
                        .ok_or_else(|| {
                            Error::msg("review request_id must be a non-empty string")
                        })?;
                    if let Some(previous) = events.iter().find(|event| {
                        event
                            .signal_type
                            .as_ref()
                            .is_some_and(|kind| kind.as_str() == "repair_requested")
                            && event
                                .payload
                                .get("request_id")
                                .and_then(|value| value.as_str())
                                == Some(id)
                    }) {
                        if previous.from != draft.from
                            || previous.payload.get("text") != draft.payload.get("text")
                            || previous.payload.get("target") != draft.payload.get("target")
                        {
                            return Err(Error::msg(
                                "review request_id was already used for another request",
                            ));
                        }
                        return Ok(Decision::Duplicate(Box::new(previous.clone())));
                    }
                }
                if self.status != ReviewStatus::Open {
                    return Err(Error::msg(
                        "review is terminal; further repairs are blocked",
                    ));
                }
                if self
                    .max_repairs
                    .is_some_and(|maximum| self.repairs >= maximum)
                {
                    return Ok(Decision::Exhausted);
                }
                draft.payload["repair_cycle"] = serde_json::json!(self
                    .repairs
                    .checked_add(1)
                    .ok_or_else(|| Error::msg("review counter overflow"))?);
            }
            Some("review_complete") if self.status != ReviewStatus::Open => {
                return Err(Error::msg("review is already terminal"))
            }
            _ => {}
        }
        Ok(Decision::Accept)
    }
}

fn maximum(payload: &serde_json::Value) -> Result<u64> {
    payload
        .get("max_repairs")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::msg("review_policy requires non-negative integer max_repairs"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventLog;
    use crate::model::SignalType;

    fn draft(kind: &str, payload: serde_json::Value) -> EventDraft {
        EventDraft {
            crew_id: "crew".into(),
            mission_id: "mission".into(),
            kind: EventKind::Signal,
            from: if kind == "review_policy" {
                "human"
            } else {
                "reviewer"
            }
            .into(),
            to: None,
            signal_type: Some(SignalType::new(kind)),
            payload,
        }
    }

    fn repair(id: &str) -> EventDraft {
        draft(
            "repair_requested",
            serde_json::json!({"request_id":id, "target":"coder", "text":"Fix the regression"}),
        )
    }

    fn state(log: &EventLog) -> ReviewState {
        ReviewState::from_events(log.read_from(0).unwrap().iter().map(|entry| &entry.event))
            .unwrap()
    }

    #[test]
    fn concurrent_repairs_are_bounded_and_terminal_state_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(draft("review_policy", serde_json::json!({"max_repairs":3})))
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let path = dir.path().to_owned();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let log = EventLog::open(&path).unwrap();
                    barrier.wait();
                    log.append(repair(&index.to_string())).is_ok()
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|thread| thread.join().unwrap().then_some(()))
                .count(),
            3
        );
        let reopened = EventLog::open(dir.path()).unwrap();
        assert_eq!(
            state(&reopened),
            ReviewState {
                max_repairs: Some(3),
                repairs: 3,
                status: ReviewStatus::Exhausted
            }
        );
        assert!(reopened.append(repair("after-restart")).is_err());
        let events = reopened.read_from(0).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|entry| entry
                    .event
                    .signal_type
                    .as_ref()
                    .is_some_and(|kind| kind.as_str() == "review_exhausted"))
                .count(),
            1
        );
    }

    #[test]
    fn requests_are_idempotent_and_policy_cannot_reset_the_counter() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        let policy = draft("review_policy", serde_json::json!({"max_repairs":2}));
        log.append(policy.clone()).unwrap();
        let first = log.append(repair("one")).unwrap();
        let reopened = EventLog::open(dir.path()).unwrap();
        assert_eq!(reopened.append(repair("one")).unwrap().id, first.id);
        assert_eq!(state(&reopened).repairs, 1);
        assert!(reopened
            .append(draft("review_policy", serde_json::json!({"max_repairs":9})))
            .is_err());
        let mut forged = policy;
        forged.from = "coder".into();
        assert!(reopened.append(forged).is_err());
        log.append(draft(
            "review_complete",
            serde_json::json!({"text":"No findings"}),
        ))
        .unwrap();
        assert_eq!(state(&reopened).status, ReviewStatus::Complete);
        assert!(log.append(repair("two")).is_err());
    }

    #[test]
    fn legacy_crews_are_unbounded_until_a_policy_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        for index in 0..5 {
            log.append(repair(&index.to_string())).unwrap();
        }
        assert_eq!(state(&log).repairs, 5);
        assert_eq!(state(&log).max_repairs, None);
        assert!(log
            .append(draft("review_policy", serde_json::json!({"max_repairs":2})))
            .is_err());
        assert!(log
            .append(draft(
                "review_policy",
                serde_json::json!({"max_repairs":-1})
            ))
            .is_err());
        assert!(log
            .append(draft("repair_requested", serde_json::json!({"text":""})))
            .is_err());
    }

    #[test]
    fn zero_budget_allows_clean_completion_but_no_repairs() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(draft("review_policy", serde_json::json!({"max_repairs":0})))
            .unwrap();
        assert!(log.append(repair("one")).is_err());
        assert_eq!(state(&log).status, ReviewStatus::Exhausted);
        assert_eq!(state(&log).repairs, 0);
    }
}
