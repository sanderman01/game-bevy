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
