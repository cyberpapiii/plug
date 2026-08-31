#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use plug_core::config::{Config, ServerConfig, TransportType};
use plug_core::engine::Engine;

const REPRESENTATIVE_BURST: usize = 8;
const WORST_CASE_BURST: usize = 32;
const SUSTAINED_ROUNDS: usize = 10;

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

    let router = Arc::clone(engine.tool_router());
    let representative = run_scenario(&router, &request_log, REPRESENTATIVE_BURST, 1).await;
    let worst_case = run_scenario(&router, &request_log, WORST_CASE_BURST, 1).await;
    let sustained = run_scenario(
        &router,
        &request_log,
        REPRESENTATIVE_BURST,
        SUSTAINED_ROUNDS,
    )
    .await;
    let catalog_correct = router.list_resources().len() == 1
        && router.list_resource_templates().len() == 1
        && router.list_prompts().len() == 1;

    let result = serde_json::json!({
        "representative_family_calls": representative.family_calls,
        "worst_family_calls": worst_case.family_calls,
        "sustained_family_calls": sustained.family_calls,
        "representative_wall_ms": representative.wall_ms,
        "worst_wall_ms": worst_case.wall_ms,
        "sustained_wall_ms": sustained.wall_ms,
        "probe_passed": 1,
        "catalog_correct": usize::from(catalog_correct),
        "representative_burst_size": REPRESENTATIVE_BURST,
        "worst_burst_size": WORST_CASE_BURST,
        "sustained_rounds": SUSTAINED_ROUNDS,
    });
    std::fs::write(
        result_path,
        serde_json::to_vec(&result).expect("serialize result"),
    )
    .expect("write result");

    engine.shutdown().await;
}

struct ScenarioResult {
    family_calls: usize,
    wall_ms: f64,
}

async fn run_scenario(
    router: &Arc<plug_core::proxy::ToolRouter>,
    request_log: &std::path::Path,
    burst_size: usize,
    rounds: usize,
) -> ScenarioResult {
    std::fs::write(request_log, "").expect("clear request log");
    let started = Instant::now();
    for _ in 0..rounds {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..burst_size {
            let router = Arc::clone(router);
            tasks.spawn(async move { router.refresh_tools().await });
        }
        while let Some(result) = tasks.join_next().await {
            result.expect("refresh task");
        }
    }
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let family_calls = std::fs::read_to_string(request_log)
        .expect("read request log")
        .lines()
        .filter(|line| {
            matches!(
                *line,
                "resources/list" | "resources/templates/list" | "prompts/list"
            )
        })
        .count();
    ScenarioResult {
        family_calls,
        wall_ms,
    }
}
