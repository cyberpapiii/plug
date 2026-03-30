use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use dashmap::DashMap;
use directories::ProjectDirs;
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolResult, Content, GetTaskPayloadResult, Meta, RawResource, ReadResourceResult,
    ResourceContents,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ProtocolError;

pub const INLINE_RESULT_MAX_BYTES: usize = 512 * 1024;
pub const ARTIFACT_RESULT_MIN_BYTES: usize = 16 * 1024 * 1024;
pub const ARTIFACT_STORE_MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const ARTIFACT_RETENTION: Duration = Duration::from_secs(72 * 60 * 60);
const ARTIFACT_SCHEME_PREFIX: &str = "plug://artifact/";
const MANIFEST_SUFFIX: &str = "/manifest";
const CHUNK_PREFIX: &str = "/chunk/";
const ARTIFACT_CHUNK_BYTES: usize = 128 * 1024;
const PREVIEW_TEXT_CHARS: usize = 4_000;
const METADATA_FILE: &str = "metadata.json";
const PAYLOAD_FILE: &str = "result.json";

#[derive(Clone, Debug)]
pub struct ArtifactRecord {
    pub id: String,
    pub source_tool: String,
    pub original_size_bytes: usize,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub payload_path: PathBuf,
    pub materialized_path: Option<PathBuf>,
    pub chunk_count: usize,
    pub preview: String,
}

#[derive(Debug)]
pub struct ArtifactStore {
    base_dir: PathBuf,
    records: DashMap<String, ArtifactRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArtifactMetadata {
    id: String,
    source_tool: String,
    original_size_bytes: usize,
    created_at_secs: u64,
    expires_at_secs: u64,
    payload_file: String,
    materialized_file: Option<String>,
    chunk_count: usize,
    preview: String,
}

impl ArtifactStore {
    pub fn new() -> Self {
        let base_dir = ProjectDirs::from("", "", "plug")
            .map(|dirs| dirs.cache_dir().join("artifacts"))
            .unwrap_or_else(|| std::env::temp_dir().join("plug-artifacts"));
        std::fs::create_dir_all(&base_dir).ok();
        let store = Self {
            base_dir,
            records: DashMap::new(),
        };
        store.rehydrate_from_disk();
        store.prune();
        store
    }

    pub fn maybe_spill_tool_result(
        &self,
        source_tool: &str,
        result: CallToolResult,
    ) -> Result<CallToolResult, McpError> {
        self.maybe_spill_tool_result_with_limit(source_tool, result, ARTIFACT_STORE_MAX_BYTES)
    }

    fn maybe_spill_tool_result_with_limit(
        &self,
        source_tool: &str,
        result: CallToolResult,
        max_store_bytes: u64,
    ) -> Result<CallToolResult, McpError> {
        let serialized = serde_json::to_vec(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let size = serialized.len();

        if size <= INLINE_RESULT_MAX_BYTES || !should_artifactize(source_tool, &result, size) {
            return Ok(result);
        }

        if size as u64 > max_store_bytes {
            return Ok(build_unpersistable_result(
                result.is_error == Some(true),
                source_tool,
                size,
                max_store_bytes,
                build_preview(&result),
            ));
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        let artifact_dir = self.base_dir.join(&id);
        std::fs::create_dir_all(&artifact_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let payload_path = artifact_dir.join(PAYLOAD_FILE);
        std::fs::write(&payload_path, &serialized)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let created_at = SystemTime::now();
        let materialized_path = maybe_materialize_attachment(&artifact_dir, &result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let record = ArtifactRecord {
            id: id.clone(),
            source_tool: source_tool.to_string(),
            original_size_bytes: size,
            created_at,
            expires_at: created_at + ARTIFACT_RETENTION,
            payload_path: payload_path.clone(),
            materialized_path,
            chunk_count: serialized.len().div_ceil(ARTIFACT_CHUNK_BYTES),
            preview: build_preview(&result),
        };
        write_metadata(&artifact_dir, &record)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        self.records.insert(id.clone(), record.clone());
        self.prune();

        Ok(build_artifact_result(result.is_error == Some(true), &record))
    }

    pub fn read(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        let request = parse_artifact_uri(uri).ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("artifact not found: {uri}"),
            })
        })?;

        let record = self.records.get(&request.id).ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("artifact not found: {uri}"),
            })
        })?;

        if SystemTime::now() > record.expires_at {
            self.records.remove(&request.id);
            let _ = std::fs::remove_dir_all(self.base_dir.join(&request.id));
            return Err(McpError::from(ProtocolError::InvalidRequest {
                detail: format!("artifact expired: {uri}"),
            }));
        }

        let text = match request.kind {
            ArtifactRequestKind::Manifest => build_manifest_text(&record),
            ArtifactRequestKind::Chunk(index) => read_chunk_text(&record, index)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?,
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
    }

    pub fn maybe_spill_task_payload(
        &self,
        source_tool: &str,
        payload: Value,
    ) -> Result<GetTaskPayloadResult, McpError> {
        match serde_json::from_value::<CallToolResult>(payload.clone()) {
            Ok(result) => {
                let spilled = self.maybe_spill_tool_result(source_tool, result)?;
                let value = serde_json::to_value(spilled)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(GetTaskPayloadResult::new(value))
            }
            Err(_) => Ok(GetTaskPayloadResult::new(payload)),
        }
    }

    pub fn prune(&self) {
        self.prune_with_limits(SystemTime::now(), ARTIFACT_RETENTION, ARTIFACT_STORE_MAX_BYTES);
    }

    fn rehydrate_from_disk(&self) {
        for dir in collect_artifact_dirs(&self.base_dir) {
            if let Some(record) = load_record_from_dir(&dir.path) {
                self.records.insert(record.id.clone(), record);
            }
        }
    }

    fn prune_with_limits(&self, now: SystemTime, retention: Duration, max_bytes: u64) {
        let mut removed_ids = Vec::new();

        let expired_ids = self
            .records
            .iter()
            .filter(|entry| entry.expires_at <= now)
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        for id in expired_ids {
            if let Some((_, record)) = self.records.remove(&id) {
                let _ = std::fs::remove_dir_all(self.base_dir.join(&record.id));
                removed_ids.push(record.id);
            }
        }

        let mut dirs = collect_artifact_dirs(&self.base_dir);
        for dir in &dirs {
            if dir
                .modified
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > retention)
            {
                let _ = std::fs::remove_dir_all(&dir.path);
                removed_ids.push(dir.id.clone());
            }
        }

        if !removed_ids.is_empty() {
            dirs.retain(|dir| !removed_ids.iter().any(|id| id == &dir.id));
        }

        let mut total_size = dirs.iter().map(|dir| dir.size).sum::<u64>();
        if total_size > max_bytes {
            dirs.sort_by_key(|dir| dir.modified.unwrap_or(SystemTime::UNIX_EPOCH));
            for dir in dirs {
                if total_size <= max_bytes {
                    break;
                }
                total_size = total_size.saturating_sub(dir.size);
                let _ = std::fs::remove_dir_all(&dir.path);
                self.records.remove(&dir.id);
            }
        }
    }
}

fn should_artifactize(source_tool: &str, result: &CallToolResult, size: usize) -> bool {
    if size >= ARTIFACT_RESULT_MIN_BYTES {
        return true;
    }

    if source_tool.ends_with("attachment_get_data") || source_tool.ends_with("attachment_blob") {
        return true;
    }

    result.content.iter().any(content_is_artifactish)
}

fn content_is_artifactish(content: &Content) -> bool {
    if content.raw.as_image().is_some() || content.raw.as_resource().is_some() {
        return true;
    }

    if let Some(text) = content.raw.as_text() {
        if text.text.len() > 256 * 1024 && looks_like_attachment_json(&text.text) {
            return true;
        }
    }

    false
}

fn looks_like_attachment_json(text: &str) -> bool {
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    matches!(
        (
            obj.get("filename").and_then(Value::as_str),
            obj.get("encoding").and_then(Value::as_str),
            obj.get("content").and_then(Value::as_str),
        ),
        (Some(_), Some("base64" | "none"), Some(_))
    )
}

fn build_preview(result: &CallToolResult) -> String {
    let mut parts = Vec::new();

    for content in &result.content {
        if let Some(text) = content.raw.as_text() {
            if !text.text.is_empty() {
                parts.push(text.text.clone());
            }
        }
    }

    if parts.is_empty() {
        if let Some(structured) = &result.structured_content {
            parts.push(structured.to_string());
        }
    }

    let mut preview = parts.join("\n\n");
    if preview.chars().count() > PREVIEW_TEXT_CHARS {
        preview = preview.chars().take(PREVIEW_TEXT_CHARS).collect::<String>() + "…";
    }
    preview
}

fn build_artifact_result(is_error: bool, record: &ArtifactRecord) -> CallToolResult {
    let manifest_uri = artifact_manifest_uri(&record.id);
    let summary = format!(
        "Tool result for `{}` was retrieved successfully but was too large to inline safely. Full result is available at `{}`.",
        record.source_tool, manifest_uri
    );

    let resource = RawResource::new(
        manifest_uri.clone(),
        format!("{} oversized result", record.source_tool),
    )
    .with_title(format!("{} oversized result", record.source_tool))
    .with_description("Artifact manifest for an oversized tool result")
    .with_mime_type("text/markdown")
    .with_size(record.original_size_bytes.min(u32::MAX as usize) as u32);

    let mut meta = Meta::new();
    meta.0.insert(
        "plug/deliveryMode".to_string(),
        serde_json::json!("artifact"),
    );
    meta.0.insert(
        "plug/artifactUri".to_string(),
        serde_json::json!(manifest_uri),
    );
    meta.0.insert(
        "plug/originalSizeBytes".to_string(),
        serde_json::json!(record.original_size_bytes),
    );
    meta.0.insert(
        "plug/truncatedInline".to_string(),
        serde_json::json!(true),
    );
    meta.0.insert(
        "plug/sourceToolName".to_string(),
        serde_json::json!(record.source_tool),
    );
    meta.0.insert(
        "plug/localPath".to_string(),
        serde_json::json!(path_display(
            record
                .materialized_path
                .as_ref()
                .unwrap_or(&record.payload_path)
        )),
    );

    let mut content = vec![Content::text(summary), Content::resource_link(resource)];
    if !record.preview.is_empty() {
        content.push(Content::text(format!("Preview:\n{}", record.preview)));
    }

    let mut result = if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.meta = Some(meta);
    result
}

fn build_unpersistable_result(
    is_error: bool,
    source_tool: &str,
    original_size_bytes: usize,
    max_store_bytes: u64,
    preview: String,
) -> CallToolResult {
    let summary = format!(
        "Tool result for `{}` was retrieved successfully but was too large to preserve safely. The payload was {} bytes, which exceeds the artifact store budget of {} bytes. Ask the upstream server for a file path, download URL, resource link, or a smaller chunked response instead of an inline blob.",
        source_tool, original_size_bytes, max_store_bytes
    );

    let mut meta = Meta::new();
    meta.0.insert(
        "plug/deliveryMode".to_string(),
        serde_json::json!("artifact-too-large"),
    );
    meta.0.insert(
        "plug/originalSizeBytes".to_string(),
        serde_json::json!(original_size_bytes),
    );
    meta.0.insert(
        "plug/artifactStoreMaxBytes".to_string(),
        serde_json::json!(max_store_bytes),
    );
    meta.0.insert(
        "plug/truncatedInline".to_string(),
        serde_json::json!(true),
    );
    meta.0.insert(
        "plug/sourceToolName".to_string(),
        serde_json::json!(source_tool),
    );
    meta.0.insert(
        "plug/fullResultPreserved".to_string(),
        serde_json::json!(false),
    );

    let mut content = vec![Content::text(summary)];
    if !preview.is_empty() {
        content.push(Content::text(format!("Preview:\n{}", preview)));
    }

    let mut result = if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.meta = Some(meta);
    result
}

fn build_manifest_text(record: &ArtifactRecord) -> String {
    format!(
        "# Oversized Tool Result\n\nsource_tool: {}\noriginal_size_bytes: {}\nlocal_path: {}\nmaterialized_path: {}\nchunk_count: {}\nfirst_chunk_uri: {}\ncreated_at: {:?}\nexpires_at: {:?}\n\n## Preview\n{}\n",
        record.source_tool,
        record.original_size_bytes,
        path_display(&record.payload_path),
        record
            .materialized_path
            .as_ref()
            .map(|path| path_display(path))
            .unwrap_or_else(|| "none".to_string()),
        record.chunk_count,
        artifact_chunk_uri(&record.id, 0),
        record.created_at,
        record.expires_at,
        record.preview
    )
}

fn artifact_manifest_uri(id: &str) -> String {
    format!("{ARTIFACT_SCHEME_PREFIX}{id}{MANIFEST_SUFFIX}")
}

fn artifact_chunk_uri(id: &str, index: usize) -> String {
    format!("{ARTIFACT_SCHEME_PREFIX}{id}{CHUNK_PREFIX}{index}")
}

enum ArtifactRequestKind {
    Manifest,
    Chunk(usize),
}

struct ArtifactRequest {
    id: String,
    kind: ArtifactRequestKind,
}

fn parse_artifact_uri(uri: &str) -> Option<ArtifactRequest> {
    if !uri.starts_with(ARTIFACT_SCHEME_PREFIX) {
        return None;
    }
    let rest = &uri[ARTIFACT_SCHEME_PREFIX.len()..];
    let (id, suffix) = rest.split_once('/')?;
    let kind = if suffix == "manifest" {
        ArtifactRequestKind::Manifest
    } else if let Some(chunk) = suffix.strip_prefix("chunk/") {
        ArtifactRequestKind::Chunk(chunk.parse().ok()?)
    } else {
        return None;
    };
    Some(ArtifactRequest {
        id: id.to_string(),
        kind,
    })
}

fn path_display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_chunk_text(record: &ArtifactRecord, index: usize) -> anyhow::Result<String> {
    let payload = std::fs::read(&record.payload_path)?;
    let start = index
        .checked_mul(ARTIFACT_CHUNK_BYTES)
        .ok_or_else(|| anyhow::anyhow!("artifact chunk index overflow"))?;
    if start >= payload.len() {
        anyhow::bail!("artifact chunk index out of range");
    }
    let end = (start + ARTIFACT_CHUNK_BYTES).min(payload.len());
    Ok(String::from_utf8_lossy(&payload[start..end]).into_owned())
}

fn maybe_materialize_attachment(
    artifact_dir: &Path,
    result: &CallToolResult,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(text) = result
        .content
        .iter()
        .find_map(|content| content.raw.as_text().map(|text| text.text.as_str()))
    else {
        return Ok(None);
    };

    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
        return Ok(None);
    };

    let Some(filename) = obj.get("filename").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(content) = obj.get("content").and_then(Value::as_str) else {
        return Ok(None);
    };
    let encoding = obj.get("encoding").and_then(Value::as_str).unwrap_or("none");

    let bytes = match encoding {
        "base64" => base64::engine::general_purpose::STANDARD.decode(content)?,
        "none" => content.as_bytes().to_vec(),
        _ => return Ok(None),
    };

    let attachment_path = artifact_dir.join(sanitize_filename(filename));
    std::fs::write(&attachment_path, bytes)?;
    Ok(Some(attachment_path))
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '\0' => '_',
            _ => ch,
        })
        .collect()
}

fn write_metadata(artifact_dir: &Path, record: &ArtifactRecord) -> anyhow::Result<()> {
    let metadata = ArtifactMetadata {
        id: record.id.clone(),
        source_tool: record.source_tool.clone(),
        original_size_bytes: record.original_size_bytes,
        created_at_secs: to_unix_secs(record.created_at)?,
        expires_at_secs: to_unix_secs(record.expires_at)?,
        payload_file: record
            .payload_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        materialized_file: record
            .materialized_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned()),
        chunk_count: record.chunk_count,
        preview: record.preview.clone(),
    };
    std::fs::write(
        artifact_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    Ok(())
}

fn load_record_from_dir(dir: &Path) -> Option<ArtifactRecord> {
    let metadata_path = dir.join(METADATA_FILE);
    if metadata_path.exists() {
        let bytes = std::fs::read(&metadata_path).ok()?;
        let metadata: ArtifactMetadata = serde_json::from_slice(&bytes).ok()?;
        let payload_path = dir.join(metadata.payload_file);
        let materialized_path = metadata.materialized_file.map(|file| dir.join(file));
        return Some(ArtifactRecord {
            id: metadata.id,
            source_tool: metadata.source_tool,
            original_size_bytes: metadata.original_size_bytes,
            created_at: from_unix_secs(metadata.created_at_secs),
            expires_at: from_unix_secs(metadata.expires_at_secs),
            payload_path,
            materialized_path,
            chunk_count: metadata.chunk_count,
            preview: metadata.preview,
        });
    }

    let payload_path = dir.join(PAYLOAD_FILE);
    if !payload_path.exists() {
        return None;
    }
    let payload = std::fs::read(&payload_path).ok()?;
    let result: CallToolResult = serde_json::from_slice(&payload).ok()?;
    let materialized_path = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.is_file() && path.file_name().is_some_and(|name| name != PAYLOAD_FILE));

    let modified = std::fs::metadata(&payload_path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .unwrap_or_else(SystemTime::now);
    let id = dir.file_name()?.to_string_lossy().into_owned();
    Some(ArtifactRecord {
        id,
        source_tool: "unknown".to_string(),
        original_size_bytes: payload.len(),
        created_at: modified,
        expires_at: modified + ARTIFACT_RETENTION,
        payload_path,
        materialized_path,
        chunk_count: payload.len().div_ceil(ARTIFACT_CHUNK_BYTES),
        preview: build_preview(&result),
    })
}

fn to_unix_secs(time: SystemTime) -> anyhow::Result<u64> {
    Ok(time.duration_since(SystemTime::UNIX_EPOCH)?.as_secs())
}

fn from_unix_secs(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

#[derive(Debug)]
struct ArtifactDirInfo {
    id: String,
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
}

fn collect_artifact_dirs(base_dir: &Path) -> Vec<ArtifactDirInfo> {
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            let modified = std::fs::metadata(&path).ok().and_then(|meta| meta.modified().ok());
            let size = directory_size(&path);
            Some(ArtifactDirInfo {
                id,
                path,
                modified,
                size,
            })
        })
        .collect()
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };

    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                directory_size(&path)
            } else {
                std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plug-artifacts-test-{}-{}",
            name,
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_store(base_dir: PathBuf) -> ArtifactStore {
        ArtifactStore {
            base_dir,
            records: DashMap::new(),
        }
    }

    #[test]
    fn prune_removes_expired_in_memory_records() {
        let base_dir = temp_dir("expired");
        let store = test_store(base_dir.clone());
        let artifact_dir = base_dir.join("expired-id");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(artifact_dir.join("result.json"), b"{}").unwrap();
        store.records.insert(
            "expired-id".to_string(),
            ArtifactRecord {
                id: "expired-id".to_string(),
                source_tool: "tool".to_string(),
                original_size_bytes: 2,
                created_at: SystemTime::UNIX_EPOCH,
                expires_at: SystemTime::UNIX_EPOCH,
                payload_path: artifact_dir.join("result.json"),
                materialized_path: None,
                chunk_count: 1,
                preview: String::new(),
            },
        );

        store.prune_with_limits(SystemTime::now(), Duration::from_secs(1), u64::MAX);

        assert!(store.records.is_empty());
        assert!(!artifact_dir.exists());
        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn prune_evicts_oldest_dirs_when_over_size_limit() {
        let base_dir = temp_dir("size");
        let old_dir = base_dir.join("a-old");
        let new_dir = base_dir.join("b-new");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(old_dir.join("result.json"), vec![0_u8; 12]).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(new_dir.join("result.json"), vec![0_u8; 12]).unwrap();

        let store = test_store(base_dir.clone());
        store.prune_with_limits(SystemTime::now(), Duration::from_secs(60 * 60), 12);

        assert!(!old_dir.exists(), "oldest artifact should be evicted first");
        assert!(new_dir.exists(), "newest artifact should remain");
        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn rehydrate_loads_metadata_back_into_index() {
        let base_dir = temp_dir("rehydrate");
        let store = test_store(base_dir.clone());
        let result = CallToolResult::success(vec![Content::text(
            "R".repeat(ARTIFACT_RESULT_MIN_BYTES + 1),
        )]);
        let spilled = store
            .maybe_spill_tool_result("rehydrate_tool", result)
            .expect("spill result");
        let uri = spilled
            .content
            .iter()
            .find_map(|content| content.raw.as_resource_link())
            .expect("artifact resource_link")
            .uri
            .clone();

        let rehydrated = test_store(base_dir.clone());
        rehydrated.rehydrate_from_disk();
        let manifest = rehydrated.read(&uri).expect("read rehydrated manifest");
        assert_eq!(manifest.contents.len(), 1);

        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn oversized_single_result_returns_clear_non_artifact_fallback() {
        let base_dir = temp_dir("oversized-single");
        let store = test_store(base_dir.clone());
        let result = CallToolResult::success(vec![Content::text("X".repeat(2_000_000))]);

        let spilled = store
            .maybe_spill_tool_result_with_limit("Mock__attachment_get_data", result, 1_024)
            .expect("fallback result");

        assert_eq!(spilled.is_error, Some(false));
        let text = spilled.content[0]
            .raw
            .as_text()
            .expect("text content")
            .text
            .clone();
        assert!(text.contains("too large to preserve safely"));
        assert!(text.contains("exceeds the artifact store budget"));
        assert_eq!(
            spilled
                .meta
                .as_ref()
                .and_then(|meta| meta.0.get("plug/deliveryMode"))
                .and_then(|value| value.as_str()),
            Some("artifact-too-large")
        );
        assert!(store.records.is_empty());

        let _ = std::fs::remove_dir_all(base_dir);
    }
}
