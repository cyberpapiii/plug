//! Plug-owned wire model for the legacy SEP-1686 task protocol.
//!
//! RMCP 3.x implements the incompatible SEP-2663 task extension. Plug keeps
//! these types at the legacy adapter boundary so upgrading the SDK cannot
//! silently change the JSON spoken to existing clients and servers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use rmcp::model::{CallToolRequestParams, ClientRequest, CustomRequest, ServerResult};

pub type Meta = serde_json::Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u64>,
}

impl TaskMetadata {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_ttl(mut self, ttl: u64) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub task_id: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    pub created_at: String,
    pub last_updated_at: String,
    pub ttl: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_interval: Option<u64>,
}

impl Task {
    pub fn new(
        task_id: String,
        status: TaskStatus,
        created_at: String,
        last_updated_at: String,
    ) -> Self {
        Self {
            task_id,
            status,
            status_message: None,
            created_at,
            last_updated_at,
            ttl: None,
            poll_interval: None,
        }
    }
    pub fn with_status_message(mut self, value: impl Into<String>) -> Self {
        self.status_message = Some(value.into());
        self
    }
    pub fn with_ttl(mut self, value: u64) -> Self {
        self.ttl = Some(value);
        self
    }
    pub fn with_poll_interval(mut self, value: u64) -> Self {
        self.poll_interval = Some(value);
        self
    }
}

impl From<&rmcp::model::Task> for Task {
    fn from(value: &rmcp::model::Task) -> Self {
        Self {
            task_id: value.task_id.clone(),
            status: match value.status {
                rmcp::model::TaskStatus::Working => TaskStatus::Working,
                rmcp::model::TaskStatus::InputRequired => TaskStatus::InputRequired,
                rmcp::model::TaskStatus::Completed => TaskStatus::Completed,
                rmcp::model::TaskStatus::Failed => TaskStatus::Failed,
                rmcp::model::TaskStatus::Cancelled => TaskStatus::Cancelled,
                other => {
                    tracing::warn!(?other, "unknown rmcp TaskStatus; coercing to Failed");
                    TaskStatus::Failed
                }
            },
            status_message: value.status_message.clone(),
            created_at: value.created_at.clone(),
            last_updated_at: value.last_updated_at.clone(),
            ttl: value.ttl_ms,
            poll_interval: value.poll_interval_ms,
        }
    }
}

impl From<&Task> for rmcp::model::Task {
    fn from(value: &Task) -> Self {
        let status = match value.status {
            TaskStatus::Working => rmcp::model::TaskStatus::Working,
            TaskStatus::InputRequired => rmcp::model::TaskStatus::InputRequired,
            TaskStatus::Completed => rmcp::model::TaskStatus::Completed,
            TaskStatus::Failed => rmcp::model::TaskStatus::Failed,
            TaskStatus::Cancelled => rmcp::model::TaskStatus::Cancelled,
        };
        let mut task = rmcp::model::Task::new(
            value.task_id.clone(),
            status,
            value.created_at.clone(),
            value.last_updated_at.clone(),
        );
        task.status_message = value.status_message.clone();
        task.ttl_ms = value.ttl;
        task.poll_interval_ms = value.poll_interval;
        task
    }
}

macro_rules! task_result {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
            pub meta: Option<Meta>,
            #[serde(flatten)]
            pub task: Task,
        }
        impl $name {
            pub fn new(task: Task) -> Self {
                Self { meta: None, task }
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskResult {
    pub task: Task,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}
impl CreateTaskResult {
    pub fn new(task: Task) -> Self {
        Self { task, meta: None }
    }
}

task_result!(GetTaskResult);
task_result!(CancelTaskResult);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksResult {
    pub tasks: Vec<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}
impl ListTasksResult {
    pub fn new(tasks: Vec<Task>) -> Self {
        Self {
            tasks,
            next_cursor: None,
            meta: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskIdParams {
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    pub task_id: String,
}
impl TaskIdParams {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            meta: None,
            task_id: task_id.into(),
        }
    }
}
pub type GetTaskParams = TaskIdParams;
pub type GetTaskPayloadParams = TaskIdParams;
pub type CancelTaskParams = TaskIdParams;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetTaskPayloadResult(pub Value);
impl GetTaskPayloadResult {
    pub fn new(value: Value) -> Self {
        Self(value)
    }
}

/// Build an RMCP custom request while preserving the legacy method and params
/// verbatim. RMCP 3.x no longer models SEP-1686's `tasks/list` and
/// `tasks/result`, and its SEP-2663 response types are deliberately
/// incompatible with the old task wire shapes. `CustomRequest` is the SDK's
/// supported raw-method escape hatch and works uniformly over stdio,
/// Streamable HTTP, and legacy SSE transports.
pub fn request(method: impl Into<String>, params: Option<Value>) -> ClientRequest {
    ClientRequest::CustomRequest(CustomRequest::new(method, params))
}

/// Convert the internal task-call marker back to SEP-1686's top-level
/// `params.task` field before the request crosses the upstream wire.
pub fn call_tool_params(mut params: CallToolRequestParams) -> Result<Value, serde_json::Error> {
    let task = params
        .meta
        .as_mut()
        .and_then(|meta| meta.remove(crate::protocol::LEGACY_TASK_REQUEST_KEY));
    if params.meta.as_ref().is_some_and(|meta| meta.is_empty()) {
        params.meta = None;
    }
    let mut value = serde_json::to_value(params)?;
    if let Some(task) = task {
        value
            .as_object_mut()
            .expect("CallToolRequestParams serializes as an object")
            .insert("task".to_string(), task);
    }
    Ok(value)
}

pub fn parse_create_result(result: ServerResult) -> Result<CreateTaskResult, serde_json::Error> {
    match result {
        ServerResult::CustomResult(result) => result.result_as(),
        ServerResult::CreateTaskResult(result) => {
            Ok(CreateTaskResult::new(Task::from(&result.task)))
        }
        other => serde_json::from_value(serde_json::to_value(other)?),
    }
}

pub fn parse_get_result(result: ServerResult) -> Result<GetTaskResult, serde_json::Error> {
    match result {
        ServerResult::CustomResult(result) => result.result_as(),
        ServerResult::GetTaskResult(result) => {
            Ok(GetTaskResult::new(Task::from(&result.task.task)))
        }
        other => serde_json::from_value(serde_json::to_value(other)?),
    }
}

pub fn parse_payload_result(
    result: ServerResult,
) -> Result<GetTaskPayloadResult, serde_json::Error> {
    match result {
        ServerResult::CustomResult(result) => Ok(GetTaskPayloadResult::new(result.0)),
        ServerResult::CallToolResult(result) => {
            Ok(GetTaskPayloadResult::new(serde_json::to_value(result)?))
        }
        other => Ok(GetTaskPayloadResult::new(serde_json::to_value(other)?)),
    }
}

pub fn parse_cancel_result(result: ServerResult) -> Result<CancelTaskResult, serde_json::Error> {
    match result {
        ServerResult::CustomResult(result) => result.result_as(),
        other => serde_json::from_value(serde_json::to_value(other)?),
    }
}

pub fn parse_list_result(result: ServerResult) -> Result<ListTasksResult, serde_json::Error> {
    match result {
        ServerResult::CustomResult(result) => result.result_as(),
        other => serde_json::from_value(serde_json::to_value(other)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_task_wire_shape_is_frozen() {
        let task = Task::new(
            "task_1".into(),
            TaskStatus::Working,
            "now".into(),
            "now".into(),
        )
        .with_ttl(1000)
        .with_poll_interval(25);
        assert_eq!(
            serde_json::to_value(CreateTaskResult::new(task)).unwrap(),
            serde_json::json!({
                "task": {"taskId":"task_1","status":"working","createdAt":"now","lastUpdatedAt":"now","ttl":1000,"pollInterval":25}
            })
        );
    }

    #[test]
    fn task_wrapped_call_restores_legacy_top_level_task_field() {
        let mut params = CallToolRequestParams::new("echo");
        params.meta.get_or_insert_with(Default::default).insert(
            crate::protocol::LEGACY_TASK_REQUEST_KEY.to_string(),
            serde_json::json!({"ttl": 123}),
        );

        assert_eq!(
            call_tool_params(params).unwrap(),
            serde_json::json!({"name":"echo","task":{"ttl":123}})
        );
    }

    #[test]
    fn legacy_create_response_is_not_lost_to_custom_result() {
        let raw = serde_json::json!({
            "task": {
                "taskId":"task_1",
                "status":"working",
                "createdAt":"now",
                "lastUpdatedAt":"now",
                "ttl":1000
            }
        });
        let result = parse_create_result(ServerResult::CustomResult(
            rmcp::model::CustomResult::new(raw),
        ))
        .unwrap();
        assert_eq!(result.task.task_id, "task_1");
        assert_eq!(result.task.ttl, Some(1000));
    }
}
