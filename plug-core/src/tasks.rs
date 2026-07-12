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
const DEFAULT_MAX_COMPLETED_TASKS_PER_OWNER: usize = 100;
const DEFAULT_STALE_IN_FLIGHT_TTL_MS: u64 = 24 * 60 * 60 * 1000;

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
pub enum TaskUpstreamRef {
    Request {
        server_id: String,
        request_id: RequestId,
    },
    Task {
        server_id: String,
        task_id: String,
    },
}

struct TaskRecord {
    task: Task,
    owner: TaskOwner,
    result: Option<Value>,
    abort_handle: Option<JoinHandle<()>>,
    upstream: Option<TaskUpstreamRef>,
    last_touched: Instant,
    /// Set by `mark_cancelled` when a cancellation arrives before the
    /// dispatching task has recorded an upstream request/task ref (i.e.
    /// there's nothing yet to forward a cancel notification to). Consumed by
    /// `set_upstream_request` once the ref lands, so the cancel is replayed
    /// upstream instead of silently dropped. Mirrors the foreground
    /// `pending_cancel_reason` guard in `proxy/mod.rs`'s
    /// `attach_upstream_request_id`.
    pending_cancel_reason: Option<String>,
}

type CancelledTaskParts = (Task, Option<TaskUpstreamRef>, Option<JoinHandle<()>>);

/// Per-owner lifecycle state: how many `enqueue_tool_task` calls are
/// currently in flight for the owner, and whether the owner was torn down
/// (`cleanup_owner`) while any of them were. Entries exist only while
/// `in_flight_creates > 0` — `end_create` removes the entry (tombstone
/// included) when the count returns to zero, so the ledger cannot grow
/// per-owner forever.
#[derive(Debug, Default)]
struct OwnerLifecycleState {
    in_flight_creates: usize,
    torn_down: bool,
}

/// Owner-lifecycle ledger closing the create-vs-teardown race: an enqueue
/// registers itself here (under the async `TaskStore` lock) *before* any
/// upstream await, so a `cleanup_owner` that runs during the round trip can
/// leave a tombstone that makes the late `create`/`create_passthrough`
/// refuse to insert a record for the already-torn-down owner.
///
/// The ledger sits behind its own std mutex (not the async store lock)
/// solely so the RAII [`OwnerCreateGuard`] can decrement synchronously from
/// `Drop`. Every compound check-then-act on it — cleanup's count-check +
/// tombstone record, create's tombstone-check + record insert — still runs
/// while the caller holds the async `TaskStore` lock, which is what makes
/// those pairs mutually atomic. No `.await` ever happens while the std
/// mutex is held.
#[derive(Debug, Default)]
struct OwnerLifecycleLedger {
    states: std::sync::Mutex<HashMap<TaskOwner, OwnerLifecycleState>>,
}

impl OwnerLifecycleLedger {
    fn lock_states(&self) -> std::sync::MutexGuard<'_, HashMap<TaskOwner, OwnerLifecycleState>> {
        // The critical sections are single-step map updates; on the poison
        // path the state is still coherent, so recover rather than cascade
        // the panic through teardown.
        self.states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn begin_create(self: &Arc<Self>, owner: &TaskOwner) -> OwnerCreateGuard {
        self.lock_states()
            .entry(owner.clone())
            .or_default()
            .in_flight_creates += 1;
        OwnerCreateGuard {
            ledger: Arc::clone(self),
            owner: owner.clone(),
        }
    }

    fn end_create(&self, owner: &TaskOwner) {
        let mut states = self.lock_states();
        if let Some(state) = states.get_mut(owner) {
            state.in_flight_creates = state.in_flight_creates.saturating_sub(1);
            if state.in_flight_creates == 0 {
                // Removing the entry is what keeps memory bounded: no owner
                // key outlives its last in-flight create, and the tombstone
                // dies with the entry — so a fresh create for the same owner
                // key after everything resolved starts clean.
                states.remove(owner);
            }
        }
    }

    fn tombstone_if_in_flight(&self, owner: &TaskOwner) {
        if let Some(state) = self.lock_states().get_mut(owner)
            && state.in_flight_creates > 0
        {
            state.torn_down = true;
        }
    }

    fn is_tombstoned(&self, owner: &TaskOwner) -> bool {
        self.lock_states()
            .get(owner)
            .is_some_and(|state| state.torn_down)
    }
}

/// RAII registration of one in-flight task create for an owner. Obtained
/// from [`TaskStore::begin_owner_create`] at `enqueue_tool_task` entry;
/// dropping it (on any path — success, error, panic unwind) deregisters the
/// create and clears the owner's tombstone once no creates remain in
/// flight.
#[must_use = "dropping the guard immediately deregisters the in-flight create"]
pub struct OwnerCreateGuard {
    ledger: Arc<OwnerLifecycleLedger>,
    owner: TaskOwner,
}

impl Drop for OwnerCreateGuard {
    fn drop(&mut self) {
        self.ledger.end_create(&self.owner);
    }
}

pub struct TaskStore {
    tasks: HashMap<String, TaskRecord>,
    next_task_id: u64,
    lifecycle: Arc<OwnerLifecycleLedger>,
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
            lifecycle: Arc::new(OwnerLifecycleLedger::default()),
        }
    }

    /// Registers an in-flight create for `owner`. Call this (under the
    /// store lock) before any upstream await in an enqueue path, so a
    /// concurrent `cleanup_owner` knows a create may land late and leaves a
    /// tombstone for it. The returned guard deregisters on drop.
    pub fn begin_owner_create(&self, owner: &TaskOwner) -> OwnerCreateGuard {
        self.lifecycle.begin_create(owner)
    }

    /// Refuses record creation for an owner that was torn down while this
    /// create was in flight (see [`OwnerLifecycleLedger`]). Without this,
    /// a create landing after `cleanup_owner` would insert a Working record
    /// nothing ever cleans up until the stale-in-flight TTL.
    fn ensure_owner_accepts_creates(&self, owner: &TaskOwner) -> Result<(), McpError> {
        if self.lifecycle.is_tombstoned(owner) {
            return Err(McpError::new(
                ErrorCode::INVALID_REQUEST,
                "session closed during task creation",
                None,
            ));
        }
        Ok(())
    }

    pub fn create(&mut self, owner: TaskOwner, name: &str) -> Result<Task, McpError> {
        self.ensure_owner_accepts_creates(&owner)?;
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
                pending_cancel_reason: None,
            },
        );

        self.enforce_owner_completed_retention();
        Ok(task)
    }

    pub fn attach_abort_handle(&mut self, task_id: &str, handle: JoinHandle<()>) {
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.abort_handle = Some(handle);
        }
    }

    /// Records the upstream request/task ref for a task once dispatch has it
    /// in flight. If a cancellation arrived while the ref was still unset
    /// (see `mark_cancelled`), returns the pending cancel reason so the
    /// caller can replay `notify_cancelled` upstream now that there's a
    /// request id to target — otherwise the cancel would be silently
    /// dropped, and the upstream would run the call to completion for a
    /// result nobody wants.
    pub fn set_upstream_request(
        &mut self,
        task_id: &str,
        upstream: TaskUpstreamRef,
    ) -> Option<String> {
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.upstream = Some(upstream);
            record.last_touched = Instant::now();
            return record.pending_cancel_reason.take();
        }
        None
    }

    pub fn create_passthrough(
        &mut self,
        owner: TaskOwner,
        name: &str,
        upstream_task: &Task,
        upstream: TaskUpstreamRef,
    ) -> Result<Task, McpError> {
        self.ensure_owner_accepts_creates(&owner)?;
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
                pending_cancel_reason: None,
            },
        );

        self.enforce_owner_completed_retention();
        Ok(task)
    }

    pub fn complete(&mut self, task_id: &str, result: Value) {
        if let Some(record) = self.tasks.get_mut(task_id) {
            if is_terminal(&record.task.status) {
                record.last_touched = Instant::now();
                return;
            }
            record.task.status = TaskStatus::Completed;
            record.task.status_message = Some("Completed".to_string());
            record.task.last_updated_at = rmcp::task_manager::current_timestamp();
            record.result = Some(result);
            record.abort_handle = None;
            record.upstream = None;
            record.last_touched = Instant::now();
        }
        self.enforce_owner_completed_retention();
    }

    pub fn fail(&mut self, task_id: &str, message: String) {
        if let Some(record) = self.tasks.get_mut(task_id) {
            if is_terminal(&record.task.status) {
                record.last_touched = Instant::now();
                return;
            }
            record.task.status = TaskStatus::Failed;
            record.task.status_message = Some(message);
            record.task.last_updated_at = rmcp::task_manager::current_timestamp();
            record.abort_handle = None;
            record.upstream = None;
            record.last_touched = Instant::now();
        }
        self.enforce_owner_completed_retention();
    }

    pub fn mark_cancelled(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<CancelledTaskParts, McpError> {
        self.prune_expired();
        let (task, upstream, handle) = {
            let record = self
                .tasks
                .get_mut(task_id)
                .ok_or_else(|| task_not_found(task_id))?;
            ensure_owner(owner, record)?;

            if is_terminal(&record.task.status) {
                record.last_touched = Instant::now();
                return Ok((record.task.clone(), None, None));
            }

            record.task.status = TaskStatus::Cancelled;
            record.task.status_message = Some("Cancelled".to_string());
            record.task.last_updated_at = rmcp::task_manager::current_timestamp();
            record.result = None;
            record.last_touched = Instant::now();

            let upstream = record.upstream.take();
            if upstream.is_none() {
                // Dispatch hasn't recorded an upstream request/task ref yet —
                // there's nothing to send `notify_cancelled` to right now.
                // Stash the reason so `set_upstream_request` can replay it
                // once the ref lands, instead of the upstream call running to
                // completion for a result nobody wants.
                record.pending_cancel_reason = Some("task cancelled".to_string());
            }

            (record.task.clone(), upstream, record.abort_handle.take())
        };
        self.enforce_owner_completed_retention();
        Ok((task, upstream, handle))
    }

    pub fn cache_result_for_owner(
        &mut self,
        owner: &TaskOwner,
        task_id: &str,
        result: Value,
    ) -> Result<(), McpError> {
        self.prune_expired();
        let record = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| task_not_found(task_id))?;
        ensure_owner(owner, record)?;

        record.result = Some(result);
        record.task.status = TaskStatus::Completed;
        record.task.status_message = Some("Completed".to_string());
        record.task.last_updated_at = rmcp::task_manager::current_timestamp();
        record.upstream = None;
        record.abort_handle = None;
        record.last_touched = Instant::now();

        self.enforce_owner_completed_retention();
        Ok(())
    }

    /// Drains every task record owned by `owner` and returns the live parts
    /// (`abort_handle`, `upstream`) each one held, so the caller can actually
    /// stop execution instead of just discarding the bookkeeping record.
    ///
    /// A dropped `JoinHandle` detaches its task rather than stopping it, and
    /// a dropped `TaskUpstreamRef` means no cancellation is ever sent
    /// upstream — so returning these parts (instead of letting them drop
    /// here) is what lets the caller abort local execution and forward
    /// cancellation upstream. Already-terminal records naturally yield
    /// `(None, None)` since `complete`/`fail`/`mark_cancelled` already clear
    /// both fields, so they no-op for the caller.
    ///
    /// If the owner has creates in flight (registered via
    /// [`Self::begin_owner_create`]), a tombstone is recorded so those
    /// creates cannot insert a record for this now-torn-down owner when they
    /// resolve. The tombstone is cleared once the last in-flight create's
    /// guard drops.
    pub fn cleanup_owner(
        &mut self,
        owner: &TaskOwner,
    ) -> Vec<(Option<TaskUpstreamRef>, Option<JoinHandle<()>>)> {
        self.lifecycle.tombstone_if_in_flight(owner);
        let owned_ids: Vec<String> = self
            .tasks
            .iter()
            .filter(|(_, record)| &record.owner == owner)
            .map(|(task_id, _)| task_id.clone())
            .collect();

        owned_ids
            .into_iter()
            .filter_map(|task_id| self.tasks.remove(&task_id))
            .map(|record| (record.upstream, record.abort_handle))
            .collect()
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

        if is_terminal(&record.task.status) && record.task.status != upstream_task.status {
            record.last_touched = Instant::now();
            return Ok(record.task.clone());
        }

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
        let record = self
            .tasks
            .get(task_id)
            .ok_or_else(|| task_not_found(task_id))?;
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
            TaskStatus::Working | TaskStatus::InputRequired => {
                now.duration_since(record.last_touched)
                    < Duration::from_millis(DEFAULT_STALE_IN_FLIGHT_TTL_MS)
            }
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => {
                let ttl_ms = record.task.ttl.unwrap_or(DEFAULT_TASK_TTL_MS);
                now.duration_since(record.last_touched) < Duration::from_millis(ttl_ms)
            }
        });
    }

    fn enforce_owner_completed_retention(&mut self) {
        let mut completed_by_owner: HashMap<TaskOwner, Vec<(String, Instant)>> = HashMap::new();
        for (task_id, record) in &self.tasks {
            if is_terminal(&record.task.status) {
                completed_by_owner
                    .entry(record.owner.clone())
                    .or_default()
                    .push((task_id.clone(), record.last_touched));
            }
        }

        for completed in completed_by_owner.values_mut() {
            if completed.len() <= DEFAULT_MAX_COMPLETED_TASKS_PER_OWNER {
                continue;
            }
            completed.sort_by_key(|(_, touched)| *touched);
            let drop_count = completed.len() - DEFAULT_MAX_COMPLETED_TASKS_PER_OWNER;
            for (task_id, _) in completed.iter().take(drop_count) {
                self.tasks.remove(task_id);
            }
        }
    }
}

fn is_terminal(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
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
        let task = store
            .create(owner_a.clone(), "Mock__echo")
            .expect("create task");
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

        store
            .create(owner_a.clone(), "Mock__echo")
            .expect("create task");
        store
            .create(owner_a.clone(), "Mock__echo")
            .expect("create task");
        store
            .create(owner_b.clone(), "Mock__echo")
            .expect("create task");

        let list_a = store.list_for_owner(&owner_a, None);
        let list_b = store.list_for_owner(&owner_b, None);
        assert_eq!(list_a.tasks.len(), 2);
        assert_eq!(list_b.tasks.len(), 1);
    }

    #[test]
    fn terminal_task_states_are_monotonic() {
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("stdio:a"));
        let task = store
            .create(owner.clone(), "Mock__echo")
            .expect("create task");

        store.complete(&task.task_id, serde_json::json!({"ok": true}));
        let completed = store
            .get_info_for_owner(&owner, &task.task_id)
            .expect("task info after completion");
        assert_eq!(completed.task.status, TaskStatus::Completed);

        let cancelled = store
            .mark_cancelled(&owner, &task.task_id)
            .expect("cancel completed task");
        assert_eq!(cancelled.0.status, TaskStatus::Completed);

        store.fail(&task.task_id, "late failure".to_string());
        let after_fail = store
            .get_info_for_owner(&owner, &task.task_id)
            .expect("task info after late failure");
        assert_eq!(after_fail.task.status, TaskStatus::Completed);
        assert!(store.get_result_for_owner(&owner, &task.task_id).is_ok());
    }

    #[test]
    fn cleanup_owner_removes_all_owned_tasks() {
        let mut store = TaskStore::new();
        let owner_a = TaskOwner::new(Arc::<str>::from("ipc:a"));
        let owner_b = TaskOwner::new(Arc::<str>::from("ipc:b"));

        let task_a = store
            .create(owner_a.clone(), "Mock__echo")
            .expect("create task");
        let task_b = store
            .create(owner_b.clone(), "Mock__echo")
            .expect("create task");

        store.cleanup_owner(&owner_a);

        assert!(store.get_info_for_owner(&owner_a, &task_a.task_id).is_err());
        assert!(store.get_info_for_owner(&owner_b, &task_b.task_id).is_ok());
    }

    #[tokio::test]
    async fn cleanup_owner_returns_live_parts_so_the_caller_can_stop_execution() {
        // `cleanup_owner` must hand back the abort handle and upstream ref it
        // is about to drop, not silently discard them — a bare `retain`
        // would drop a still-running `JoinHandle` (which detaches rather
        // than stops it) and the upstream ref (so no cancel is ever sent).
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("ipc:a"));

        let running = store
            .create(owner.clone(), "Mock__echo")
            .expect("create task");
        let handle = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        store.attach_abort_handle(&running.task_id, handle);
        let pending = store.set_upstream_request(
            &running.task_id,
            TaskUpstreamRef::Request {
                server_id: "mock".to_string(),
                request_id: rmcp::model::RequestId::Number(1),
            },
        );
        assert_eq!(pending, None);

        let completed = store
            .create(owner.clone(), "Mock__echo")
            .expect("create task");
        store.complete(&completed.task_id, serde_json::json!({"ok": true}));

        let mut drained = store.cleanup_owner(&owner);
        assert_eq!(drained.len(), 2);

        // Order isn't part of the contract; sort by whether an upstream ref
        // is present so the assertions below are deterministic.
        drained.sort_by_key(|(upstream, _)| upstream.is_none());

        let (running_upstream, running_handle) = drained.remove(0);
        assert!(
            matches!(running_upstream, Some(TaskUpstreamRef::Request { .. })),
            "still-running task's upstream ref must be returned, not dropped"
        );
        let running_handle = running_handle.expect("still-running task's handle must be returned");
        running_handle.abort();
        assert!(running_handle.await.unwrap_err().is_cancelled());

        let (completed_upstream, completed_handle) = drained.remove(0);
        assert!(
            completed_upstream.is_none(),
            "completed task has no upstream ref left to forward"
        );
        assert!(
            completed_handle.is_none(),
            "completed task has no abort handle left to abort"
        );

        assert!(store.get_info_for_owner(&owner, &running.task_id).is_err());
        assert!(
            store
                .get_info_for_owner(&owner, &completed.task_id)
                .is_err()
        );
    }

    #[test]
    fn cached_result_is_returned_locally() {
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("ipc:a"));
        let task = store
            .create_passthrough(
                owner.clone(),
                "Mock__echo",
                &Task::new(
                    "upstream-task-1".to_string(),
                    TaskStatus::Working,
                    rmcp::task_manager::current_timestamp(),
                    rmcp::task_manager::current_timestamp(),
                ),
                TaskUpstreamRef::Task {
                    server_id: "mock".to_string(),
                    task_id: "upstream-task-1".to_string(),
                },
            )
            .expect("create passthrough task");

        store
            .cache_result_for_owner(&owner, &task.task_id, serde_json::json!({"ok": true}))
            .expect("cache result");

        let payload = store
            .get_result_for_owner(&owner, &task.task_id)
            .expect("local cached result");
        assert_eq!(payload.0, serde_json::json!({"ok": true}));
        assert!(
            store
                .upstream_for_owner(&owner, &task.task_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn set_upstream_request_replays_a_cancel_that_arrived_before_it() {
        // Regression guard for the task-cancellation window: if a task is
        // cancelled before dispatch has recorded an upstream request ref,
        // `mark_cancelled` has no ref to forward `notify_cancelled` to and
        // used to drop the cancellation on the floor — the upstream would
        // run the call to completion for a result nobody wants. The fix
        // stashes a pending-cancel reason on the record and replays it via
        // `set_upstream_request`'s return value once the ref lands.
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("ipc:a"));
        let task = store
            .create(owner.clone(), "Mock__echo")
            .expect("create task");

        // Cancel arrives first — no upstream ref has been set yet.
        let (cancelled_task, upstream, _handle) = store
            .mark_cancelled(&owner, &task.task_id)
            .expect("cancel task with no upstream ref yet");
        assert_eq!(cancelled_task.status, TaskStatus::Cancelled);
        assert!(
            upstream.is_none(),
            "no upstream ref should have been recorded yet"
        );

        // Dispatch now records the upstream ref (as it would right after
        // `send_cancellable_request` succeeds) — this must report the
        // pending cancel so the caller can replay `notify_cancelled`.
        let pending = store.set_upstream_request(
            &task.task_id,
            TaskUpstreamRef::Request {
                server_id: "mock".to_string(),
                request_id: rmcp::model::RequestId::Number(1),
            },
        );
        assert_eq!(
            pending,
            Some("task cancelled".to_string()),
            "expected the stashed cancel reason to be replayed"
        );

        // The pending reason is consumed, not sticky — a second call must
        // not re-report a cancel that was already replayed.
        let pending_again = store.set_upstream_request(
            &task.task_id,
            TaskUpstreamRef::Request {
                server_id: "mock".to_string(),
                request_id: rmcp::model::RequestId::Number(2),
            },
        );
        assert_eq!(pending_again, None);
    }

    #[test]
    fn set_upstream_request_reports_no_pending_cancel_for_the_ordinary_path() {
        // Sanity check for the ordinary (non-cancelled) dispatch path: no
        // cancel arrived, so recording the upstream ref must not fabricate
        // a pending-cancel reason.
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("ipc:a"));
        let task = store.create(owner, "Mock__echo").expect("create task");

        let pending = store.set_upstream_request(
            &task.task_id,
            TaskUpstreamRef::Request {
                server_id: "mock".to_string(),
                request_id: rmcp::model::RequestId::Number(1),
            },
        );
        assert_eq!(pending, None);
    }

    fn upstream_working_task(task_id: &str) -> Task {
        Task::new(
            task_id.to_string(),
            TaskStatus::Working,
            rmcp::task_manager::current_timestamp(),
            rmcp::task_manager::current_timestamp(),
        )
    }

    /// Deterministic store-layer replay of the create-vs-teardown race: a
    /// teardown that interleaves with an in-flight enqueue (guard held)
    /// must tombstone the owner so the late `create`/`create_passthrough`
    /// refuses to insert a record for the torn-down owner.
    #[test]
    fn cleanup_with_in_flight_create_tombstones_late_creates() {
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("http:racy"));

        // Enqueue entry: register the in-flight create, as `enqueue_tool_task`
        // does before its upstream await.
        let guard = store.begin_owner_create(&owner);

        // Teardown interleaves while the create is parked upstream.
        let drained = store.cleanup_owner(&owner);
        assert!(drained.is_empty(), "no records existed yet to drain");

        // The late create (either path) must be refused, not inserted.
        let create_err = store
            .create(owner.clone(), "Mock__echo")
            .expect_err("local create after teardown must be refused");
        assert_eq!(create_err.code, ErrorCode::INVALID_REQUEST);
        let passthrough_err = store
            .create_passthrough(
                owner.clone(),
                "Mock__echo",
                &upstream_working_task("upstream-task-1"),
                TaskUpstreamRef::Task {
                    server_id: "mock".to_string(),
                    task_id: "upstream-task-1".to_string(),
                },
            )
            .expect_err("passthrough create after teardown must be refused");
        assert_eq!(passthrough_err.code, ErrorCode::INVALID_REQUEST);
        assert!(store.list_for_owner(&owner, None).tasks.is_empty());

        // Tombstone hygiene: once the in-flight enqueue resolves (guard
        // drops), the tombstone is gone and a fresh create for the same
        // owner key succeeds.
        drop(guard);
        store
            .create(owner.clone(), "Mock__echo")
            .expect("fresh create after the in-flight enqueue resolved");
        assert_eq!(store.list_for_owner(&owner, None).tasks.len(), 1);
    }

    /// The tombstone must survive until the LAST in-flight create resolves —
    /// clearing it on the first guard drop would let the second late create
    /// slip an orphan record in.
    #[test]
    fn tombstone_persists_until_all_in_flight_creates_resolve() {
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("ipc:racy"));

        let guard_a = store.begin_owner_create(&owner);
        let guard_b = store.begin_owner_create(&owner);
        store.cleanup_owner(&owner);

        drop(guard_a);
        assert!(
            store.create(owner.clone(), "Mock__echo").is_err(),
            "tombstone must persist while another create is still in flight"
        );

        drop(guard_b);
        store
            .create(owner.clone(), "Mock__echo")
            .expect("tombstone cleared once the last in-flight create resolved");
    }

    /// Teardown with NO creates in flight must not leave a tombstone —
    /// otherwise a later reconnect reusing the same owner key (IPC client
    /// ids) could never create tasks again.
    #[test]
    fn cleanup_without_in_flight_creates_leaves_no_tombstone() {
        let mut store = TaskStore::new();
        let owner = TaskOwner::new(Arc::<str>::from("ipc:reused"));

        store
            .create(owner.clone(), "Mock__echo")
            .expect("create task");
        store.cleanup_owner(&owner);

        store
            .create(owner.clone(), "Mock__echo")
            .expect("create for the same owner key after a quiescent teardown");
    }
}
