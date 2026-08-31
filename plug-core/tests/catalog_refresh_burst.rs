#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use plug_core::config::{Config, ServerConfig, TransportType};
use plug_core::engine::Engine;

const BURST_SIZE: usize = 8;

fn mock_server_config(request_log: &std::path::Path) -> ServerConfig {
    ServerConfig {
        command: Some(
            plug_test_harness::mock_server_bin()
                .to_string_lossy()
                .into_owned(),
        ),
        args: vec![
            "--tools".to_string(),
            "echo".to_string(),
            "--resources".to_string(),
            "--resource-templates".to_string(),
            "--prompts".to_string(),
            "--list-delay-ms".to_string(),
            "50".to_string(),
            "--request-log-file".to_string(),
            request_log.to_string_lossy().into_owned(),
        ],
        env: HashMap::new(),
        enabled: true,
        transport: TransportType::Stdio,
        protocol_mode: Default::default(),
        url: None,
        auth_token: None,
        auth: None,
        oauth_client_id: None,
        oauth_scopes: None,
        timeout_secs: 10,
        call_timeout_secs: 5,
        max_concurrent: 8,
        health_check_interval_secs: 60,
        circuit_breaker_enabled: true,
        enrichment: false,
        tool_renames: HashMap::new(),
        tool_groups: Vec::new(),
        sandbox: None,
    }
}

#[tokio::test]
#[ignore = "measurement harness; run explicitly through ce-optimize"]
async fn catalog_refresh_burst_measurement() {
    let result_path = std::env::var_os("CE_REFRESH_RESULT")
        .map(std::path::PathBuf::from)
        .expect("CE_REFRESH_RESULT must name the output JSON file");
    let request_log = std::env::var_os("CE_REFRESH_REQUEST_LOG")
        .map(std::path::PathBuf::from)
        .expect("CE_REFRESH_REQUEST_LOG must name the request log");

    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config(&request_log));
    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    std::fs::write(&request_log, "").expect("clear startup request log");
    let router = Arc::clone(engine.tool_router());
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..BURST_SIZE {
        let router = Arc::clone(&router);
        tasks.spawn(async move { router.refresh_tools().await });
    }
    while let Some(result) = tasks.join_next().await {
        result.expect("refresh task");
    }
    let burst_wall_ms = started.elapsed().as_secs_f64() * 1000.0;

    let log = std::fs::read_to_string(&request_log).expect("read request log");
    let resource_calls = log.lines().filter(|line| *line == "resources/list").count();
    let template_calls = log
        .lines()
        .filter(|line| *line == "resources/templates/list")
        .count();
    let prompt_calls = log.lines().filter(|line| *line == "prompts/list").count();
    let family_calls = resource_calls + template_calls + prompt_calls;
    let catalog_correct = router.list_resources().len() == 1
        && router.list_resource_templates().len() == 1
        && router.list_prompts().len() == 1;

    let result = serde_json::json!({
        "family_calls": family_calls,
        "burst_wall_ms": burst_wall_ms,
        "probe_passed": 1,
        "catalog_correct": usize::from(catalog_correct),
        "resource_calls": resource_calls,
        "template_calls": template_calls,
        "prompt_calls": prompt_calls,
        "burst_size": BURST_SIZE,
    });
    std::fs::write(
        result_path,
        serde_json::to_vec(&result).expect("serialize result"),
    )
    .expect("write result");

    engine.shutdown().await;
}
