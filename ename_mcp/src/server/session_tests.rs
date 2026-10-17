//! Whole MCP sessions in one process: an rmcp client on one end of a pipe, `GameServer` on the
//! other, a fake game behind it. This is how the dynamic tools and `list_changed` are tested
//! without restarting a real client, once per lifecycle rmcp negotiates.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use rmcp::{
    ClientHandler, ClientServiceExt, ServiceExt,
    model::{CallToolRequestParams, ProtocolVersion, ServerNotification, SubscriptionFilter},
    service::{NotificationContext, RoleClient},
};
use serde_json::{Value, json};

use super::{
    GameServer,
    tests::{Responder, serve},
};
use crate::brp::BrpClient;

/// A game that is down (every method unknown) until `up` is set, then lists one instant command
/// and answers it.
async fn game(up: Arc<AtomicBool>) -> String {
    let respond: Responder = Arc::new(move |_, request: &Value| {
        if !up.load(Ordering::SeqCst) {
            return Some(json!({ "error": { "code": -32601, "message": "Method not found" } }));
        }
        Some(match request["method"].as_str().unwrap() {
            "game.command.list" => json!({ "result": { "run": "aaaaaaaa", "commands": [{
                "name": "view.read", "description": "View.", "kind": "instant",
                "scope": "development", "read_only": true,
                "params": { "type": "object", "properties": {} }, "result": {},
            }] } }),
            "game.command.call" => {
                json!({ "result": { "run": "aaaaaaaa", "result": { "view": "game" } } })
            }
            _ => json!({ "error": { "code": -32601, "message": "Method not found" } }),
        })
    });
    serve(respond).await
}

#[derive(Clone, Default)]
struct CountingClient {
    tool_changes: Arc<AtomicUsize>,
}

impl ClientHandler for CountingClient {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.tool_changes.fetch_add(1, Ordering::SeqCst);
    }
}

async fn has_tool(client: &rmcp::service::Peer<RoleClient>, name: &str) -> bool {
    client
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .any(|tool| tool.name == name)
}

async fn call_get_view(client: &rmcp::service::Peer<RoleClient>) -> Value {
    let result = client
        .call_tool(CallToolRequestParams::new("view_read"))
        .await
        .unwrap();
    result.structured_content.unwrap()
}

#[tokio::test]
async fn legacy_clients_hear_about_tools_when_the_game_starts() {
    let up = Arc::new(AtomicBool::new(false));
    let server = GameServer::new(BrpClient::new(game(up.clone()).await));
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        server.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });

    let handler = CountingClient::default();
    let changes = handler.tool_changes.clone();
    let client = handler.serve(client_io).await.unwrap();
    assert!(has_tool(client.peer(), "command_call").await);
    assert!(!has_tool(client.peer(), "view_read").await);

    up.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(10), async {
        while changes.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("no tools/list_changed after the game came up");
    assert!(has_tool(client.peer(), "view_read").await);
    assert_eq!(
        call_get_view(client.peer()).await,
        json!({ "pid": "aaaaaaaa", "view": "game" })
    );
}

#[tokio::test]
async fn subscribed_clients_hear_about_tools_when_the_game_starts() {
    let up = Arc::new(AtomicBool::new(false));
    let server = GameServer::new(BrpClient::new(game(up.clone()).await));
    let catalog = server.catalog.clone();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        server.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });

    let client = ()
        .serve_with_lifecycle(
            client_io,
            rmcp::ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();
    let mut subscription = client
        .listen(SubscriptionFilter::builder().tools_list_changed().build())
        .await
        .unwrap();
    assert!(!has_tool(client.peer(), "view_read").await);

    up.store(true, Ordering::SeqCst);
    let notification = tokio::time::timeout(Duration::from_secs(10), subscription.next())
        .await
        .expect("no tools/list_changed after the game came up")
        .unwrap()
        .unwrap();
    assert!(matches!(
        notification,
        ServerNotification::ToolListChangedNotification(_)
    ));
    assert!(has_tool(client.peer(), "view_read").await);
    assert_eq!(
        call_get_view(client.peer()).await,
        json!({ "pid": "aaaaaaaa", "view": "game" })
    );

    // Ending the subscription unregisters it, without waiting for a change to find it closed.
    assert_eq!(catalog.subscription_count(), 1);
    subscription.cancel().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while catalog.subscription_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the ended subscription is still registered");
}

#[tokio::test]
async fn a_failing_static_tool_is_a_tool_error_the_agent_can_read() {
    let up = Arc::new(AtomicBool::new(false));
    let server = GameServer::new(BrpClient::new(game(up).await));
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        server.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_io).await.unwrap();
    let result = client
        .call_tool(CallToolRequestParams::new("time_get"))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let text: String = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    assert!(text.contains("game.time.get"), "{text}");
}

#[tokio::test]
async fn static_tools_carry_new_errors_on_results_and_failures() {
    let respond: Responder = Arc::new(|_, request: &Value| {
        let log = json!({ "errors": 1, "warnings": 0, "last_sequence": 9,
                          "last_error": "bevy_render: failed to process shader" });
        Some(match request["method"].as_str().unwrap() {
            "game.pid.get" => json!({ "result": { "pid": "aaaaaaaa", "os_pid": 1, "log": log } }),
            "game.time.get" => json!({ "result": { "paused": false } }),
            _ => json!({ "error": { "code": -23401, "message": "no such entity" } }),
        })
    });
    let server = GameServer::new(BrpClient::new(serve(respond).await));
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        server.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_io).await.unwrap();

    let first = client
        .call_tool(CallToolRequestParams::new("time_get"))
        .await
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(first["log"]["new_errors"], 1, "{first}");
    let second = client
        .call_tool(CallToolRequestParams::new("time_get"))
        .await
        .unwrap()
        .structured_content
        .unwrap();
    assert!(second.get("log").is_none(), "{second}");

    // A fresh server has its own baseline, so its failure reports the same error.
    let respond: Responder = Arc::new(|_, request: &Value| {
        Some(match request["method"].as_str().unwrap() {
            "game.pid.get" => json!({ "result": { "pid": "aaaaaaaa", "os_pid": 1,
                "log": { "errors": 2, "warnings": 0, "last_error": "panic: boom" } } }),
            _ => json!({ "error": { "code": -23401, "message": "no such entity" } }),
        })
    });
    let server = GameServer::new(BrpClient::new(serve(respond).await));
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        server.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_io).await.unwrap();
    let failed = client
        .call_tool(CallToolRequestParams::new("time_get"))
        .await
        .unwrap();
    assert_eq!(failed.is_error, Some(true));
    let text: String = failed
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    assert!(
        text.contains("2 new error(s)") && text.contains("panic: boom"),
        "{text}"
    );
}

#[tokio::test]
async fn a_hung_game_is_reported_as_running_but_not_answering() {
    let url = serve(Arc::new(|_, _: &Value| None)).await;
    let port = url.rsplit_once(':').unwrap().1.to_owned();
    // This test process plays the game the record names, so it counts as alive.
    let records = tempfile::tempdir().unwrap();
    let me = crate::launch::process::info(std::process::id()).unwrap();
    let record = json!({
        "run": "aaaaaaaa", "os_pid": std::process::id(), "exe": me.exe,
        "started_unix_ms": me.start_time * 1000, "panics": 0,
    });
    std::fs::write(
        records.path().join(format!("brp-{port}.json")),
        record.to_string(),
    )
    .unwrap();
    let brp =
        BrpClient::with_timeout(url, Duration::from_millis(300)).with_record_dir(records.path());
    let server = GameServer::new(brp);
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        server.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_io).await.unwrap();

    let started = std::time::Instant::now();
    let result = client
        .call_tool(CallToolRequestParams::new("time_get"))
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(result.is_error, Some(true));
    let text: String = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    assert!(
        text.contains(&format!("is running but not answering on {port}")),
        "{text}"
    );
}

/// A launcher over an empty temporary workspace: no state directory, no game.
fn empty_lifecycle(root: &std::path::Path) -> super::Lifecycle {
    let project = crate::launch::Project {
        workspace_root: root.to_path_buf(),
        target_directory: root.join("target"),
        config: crate::launch::LaunchConfig {
            package: "editor".to_owned(),
            features: Vec::new(),
            binary: "editor".to_owned(),
        },
    };
    super::Lifecycle::new(crate::launch::Launcher::new(project), Some("test".into()))
}

#[tokio::test]
async fn the_lifecycle_tools_are_listed_with_the_game_down() {
    let root = tempfile::tempdir().unwrap();
    let up = Arc::new(AtomicBool::new(false));
    let endpoint = crate::brp::Endpoint::fixed(BrpClient::new(game(up).await));
    let server = GameServer::with_endpoint(endpoint, Some(empty_lifecycle(root.path())));
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        server.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_io).await.unwrap();
    for name in ["game_start", "game_stop", "game_restart", "game_status"] {
        assert!(has_tool(client.peer(), name).await, "{name} is not listed");
    }

    // An instance other than `default` has no port to probe, so this does not depend on
    // whether a real game runs on this machine.
    let result = client
        .call_tool(
            CallToolRequestParams::new("game_status")
                .with_arguments(json!({ "instance": "test" }).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let status = result.structured_content.unwrap();
    assert_eq!(status["state"], "stopped", "{status}");
    assert_eq!(status["yours"], json!([]), "{status}");
    assert!(
        !root.path().join("target").exists(),
        "status must not create state"
    );
}
