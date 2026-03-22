use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::ErrorData as McpError;
use rmcp::model::{
    CancelTaskResult, ErrorCode, GetTaskPayloadResult, GetTaskResult, ListTasksResult, RequestId,
    Task, TaskStatus,
};
use serde_json::Value;
use tokio::task::JoinHandle;

pub const DEFAULT_TASK_TTL_MS: u64 = 60 * 60 * 1000;
pub const DEFAULT_TASK_POLL_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskOwner {
    key: Arc<str>,
}

impl TaskOwner {
    pub fn new(key: impl Into<Arc<str>>) -> Self {
        Self { key: key.into() }
    }

    pub fn as_key(&self) -> &str {
        &self.key
    }
}

#[derive(Debug, Clone)]
pub struct TaskUpstreamRef {
    pub server_id: String,
    pub request_id: RequestId,
}

struct TaskRecord {
    task: Task,
    owner: TaskOwner,
    result: Option<Value>,
    abort_handle: Option<JoinHandle<()>>,
    upstream: Option<TaskUpstreamRef>,
    last_touched: Instant,
}

pub struct TaskStore {
    tasks: HashMap<String, TaskRecord>,
    next_task_id: u64,
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            next_task_id: 1,
        }
    }

    pub fn create(&mut self, owner: TaskOwner, name: &str) -> Task {
        self.prune_expired();
        let task_id = format!("task_{}", self.next_task_id);
        self.next_task_id += 1;

        let now = rmcp::task_manager::current_timestamp();
        let task = Task::new(task_id.clone(), TaskStatus::Working, now.clone(), now)
            .with_status_message(format!("Running {name}"))
            .with_ttl(DEFAULT_TASK_TTL_MS)
            .with_poll_interval(DEFAULT_TASK_POLL_INTERVAL_MS);

        self.tasks.insert(
            task_id,
            TaskRecord {
                task: task.clone(),
                owner,
                result: None,
                abort_handle: None,
                upstream: None,
                last_touched: Instant::now(),
            },
        );

        task
    }

    pub fn attach_abort_handle(&mut self, task_id: &str, handle: JoinHandle<()>) {
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.abort_handle = Some(handle);
        }
    }

    pub fn set_upstream_request(&mut self, task_id: &str, upstream: TaskUpstreamRef) {
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.upstream = Some(upstream);
            record.last_touched = Instant::now();
        }
    }

    pub fn create_passthrough(
        &mut self,
        owner: TaskOwner,
        name: &str,
        upstream_task: &Task,
        upstream: TaskUpstreamRef,
    ) -> Task {
        self.prune_expired();
        let task_id = format!("task_{}", self.next_task_id);
        self.next_task_id += 1;

        let mut task = upstream_task.clone();
        task.task_id = task_id.clone();
        if task.status_message.is_none() {
            task.status_message = Some(format!("Running {name}"));
        }

        self.tasks.insert(
            task_id,
            TaskRecord {
                task: task.clone(),
                owner,
                result: None,
                abort_handle: None,
                upstream: Some(upstream),
                last_touched: Instant::now(),
            },
        );

        task
    }

    pub fn complete(&mut self, task_id: &str, result: Value) {
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.task.status = TaskStatus::Completed;
            record.task.status_message = Some("Completed".to_string());
            record.task.last_updated_at = rmcp::task_manager::current_timestamp();
            record.result = Some(result);
            record.abort_handle = None;
            record.upstream = None;
            record.last_touched = Instant::now();
        }
    }

    pub fn fail(&mut self, task_id: &str, message: String) {
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.task.status = TaskStatus::Failed;
            record.task.status_message = Some(message);
            record.task.last_updated_at = rmcp::task_manager::current_timestamp();
            record.abort_handle = None;
            record.upstream = None;
            record.last_touched = Instant::now();
        }
    }

    pub fn mark_cancelled(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<(Task, Option<TaskUpstreamRef>, Option<JoinHandle<()>>), McpError> {
        self.prune_expired();
        let record = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| task_not_found(task_id))?;
        ensure_owner(owner, record)?;

        record.task.status = TaskStatus::Cancelled;
        record.task.status_message = Some("Cancelled".to_string());
        record.task.last_updated_at = rmcp::task_manager::current_timestamp();
        record.result = None;
        record.last_touched = Instant::now();

        let upstream = record.upstream.take();
        let handle = record.abort_handle.take();
        Ok((record.task.clone(), upstream, handle))
    }

    pub fn list_for_owner(
        &mut self,
        owner: &TaskOwner,
        request: Option<rmcp::model::PaginatedRequestParams>,
    ) -> ListTasksResult {
        self.prune_expired();
        let mut tasks = self
            .tasks
            .values()
            .filter(|record| record.owner == *owner)
            .map(|record| record.task.clone())
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at));

        paginate_tasks(tasks, request)
    }

    pub fn get_info_for_owner(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<GetTaskResult, McpError> {
        self.prune_expired();
        let record = self
            .tasks
            .get(task_id)
            .ok_or_else(|| task_not_found(task_id))?;
        ensure_owner(owner, record)?;
        Ok(GetTaskResult {
            meta: None,
            task: record.task.clone(),
        })
    }

    pub fn sync_from_upstream_for_owner(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
        upstream_task: &Task,
    ) -> Result<Task, McpError> {
        self.prune_expired();
        let record = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| task_not_found(task_id))?;
        ensure_owner(owner, record)?;

        record.task.status = upstream_task.status.clone();
        record.task.status_message = upstream_task.status_message.clone();
        record.task.created_at = upstream_task.created_at.clone();
        record.task.last_updated_at = upstream_task.last_updated_at.clone();
        record.task.ttl = upstream_task.ttl;
        record.task.poll_interval = upstream_task.poll_interval;
        record.last_touched = Instant::now();

        Ok(record.task.clone())
    }

    pub fn upstream_for_owner(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<Option<TaskUpstreamRef>, McpError> {
        self.prune_expired();
        let record = self.tasks.get(task_id).ok_or_else(|| task_not_found(task_id))?;
        ensure_owner(owner, record)?;
        Ok(record.upstream.clone())
    }

    pub fn get_result_for_owner(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<GetTaskPayloadResult, McpError> {
        self.prune_expired();
        let record = self
            .tasks
            .get(task_id)
            .ok_or_else(|| task_not_found(task_id))?;
        ensure_owner(owner, record)?;

        match record.task.status {
            TaskStatus::Completed => record
                .result
                .clone()
                .map(GetTaskPayloadResult::new)
                .ok_or_else(|| {
                    McpError::new(ErrorCode::INTERNAL_ERROR, "task result missing", None)
                }),
            TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Working
            | TaskStatus::InputRequired => Err(McpError::new(
                ErrorCode::INVALID_REQUEST,
                format!("task {task_id} is not in a completed state"),
                None,
            )),
        }
    }

    pub fn cancel_result_for_owner(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<CancelTaskResult, McpError> {
        self.prune_expired();
        let record = self
            .tasks
            .get(task_id)
            .ok_or_else(|| task_not_found(task_id))?;
        ensure_owner(owner, record)?;
        Ok(CancelTaskResult {
            meta: None,
            task: record.task.clone(),
        })
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        self.tasks.retain(|_, record| match record.task.status {
            TaskStatus::Working | TaskStatus::InputRequired => true,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => {
                let ttl_ms = record.task.ttl.unwrap_or(DEFAULT_TASK_TTL_MS);
                now.duration_since(record.last_touched) < Duration::from_millis(ttl_ms)
            }
        });
    }
}

fn ensure_owner(owner: &TaskOwner, record: &TaskRecord) -> Result<(), McpError> {
    if &record.owner == owner {
        Ok(())
    } else {
        Err(McpError::new(
            ErrorCode::INVALID_REQUEST,
            "task does not belong to this client/session",
            None,
        ))
    }
}

fn task_not_found(task_id: &str) -> McpError {
    McpError::new(
        ErrorCode::INVALID_REQUEST,
        format!("task not found: {task_id}"),
        None,
    )
}

fn paginate_tasks(
    tasks: Vec<Task>,
    request: Option<rmcp::model::PaginatedRequestParams>,
) -> ListTasksResult {
    const PAGE_SIZE: usize = 500;
    let total = tasks.len() as u64;
    let start = request
        .as_ref()
        .and_then(|params| params.cursor.as_ref())
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .filter(|idx| *idx < tasks.len())
        .unwrap_or(0);
    let end = usize::min(start + PAGE_SIZE, tasks.len());
    let next_cursor = (end < tasks.len()).then(|| end.to_string());

    let mut result = ListTasksResult::new(tasks[start..end].to_vec());
    result.next_cursor = next_cursor;
    result.total = Some(total);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_owner_isolated_for_info_and_result_access() {
        let mut store = TaskStore::new();
        let owner_a = TaskOwner::new(Arc::<str>::from("stdio:a"));
        let owner_b = TaskOwner::new(Arc::<str>::from("stdio:b"));
        let task = store.create(owner_a.clone(), "Mock__echo");
        store.complete(&task.task_id, serde_json::json!({"ok": true}));

        assert!(store.get_info_for_owner(&owner_a, &task.task_id).is_ok());
        assert!(store.get_result_for_owner(&owner_a, &task.task_id).is_ok());
        assert!(store.get_info_for_owner(&owner_b, &task.task_id).is_err());
        assert!(store.get_result_for_owner(&owner_b, &task.task_id).is_err());
    }

    #[test]
    fn task_list_is_scoped_to_owner() {
        let mut store = TaskStore::new();
        let owner_a = TaskOwner::new(Arc::<str>::from("stdio:a"));
        let owner_b = TaskOwner::new(Arc::<str>::from("stdio:b"));

        store.create(owner_a.clone(), "Mock__echo");
        store.create(owner_a.clone(), "Mock__echo");
        store.create(owner_b.clone(), "Mock__echo");

        let list_a = store.list_for_owner(&owner_a, None);
        let list_b = store.list_for_owner(&owner_b, None);
        assert_eq!(list_a.tasks.len(), 2);
        assert_eq!(list_b.tasks.len(), 1);
    }
}
