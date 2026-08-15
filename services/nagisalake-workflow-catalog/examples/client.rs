//! Example RPC client for testing the workflow catalog service.

use nagisalake_rpc::{Client, ClientBuilder, ClientContext, TcpConnector};
use nagisalake_workflow_catalog::{ListWorkflowsRequest, ListWorkflowsResponse};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let addr: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9090".into())
        .parse()?;

    println!("Connecting to workflow-catalog at {addr}...");

    let client: Client<ListWorkflowsRequest, ListWorkflowsResponse> = ClientBuilder::new()
        .transport(TcpConnector::new(addr))
        .connect()
        .await?;

    println!("Connected! Listing workflows...\n");

    let request = ListWorkflowsRequest {
        user_id: "test-user".into(),
        cursor:  None,
        limit:   Some(10),
    };

    let response = client.call(ClientContext::default(), request).await?;

    println!("Found {} workflows:", response.items.len());
    for workflow in response.items {
        println!("  - Workflow ID: {}", workflow.workflow_id);
        println!("    Organization: {}", workflow.organization_id);
        println!("    Version: {}", workflow.version);
        if let Some(hash) = &workflow.content_hash {
            println!("    Content Hash: {}", hash);
        }
        println!();
    }

    if response.next_cursor.is_some() {
        println!("More results available (use cursor for next page)");
    }

    Ok(())
}
