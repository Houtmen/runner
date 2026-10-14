use super::model::{Mission, Role, SessionStatus};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionLimitInfo {
    pub maximum: Option<usize>,
    pub active: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationSession {
    pub id: String,
    pub title: String,
    pub runtime: Option<String>,
    pub status: SessionStatus,
    pub role_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationSnapshot {
    pub limit: SessionLimitInfo,
    pub codex_home: String,
    pub roles: Vec<Role>,
    pub sessions: Vec<OrchestrationSession>,
    pub missions: Vec<Mission>,
}
