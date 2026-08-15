# Nagisalake Workflow Catalog Service

The first internal service extracted from the Nagisalake Hub monolith.

## Purpose

Provides read-only workflow metadata queries over `nagisalake-rpc`. The service:

- Lists workflow versions visible to a user (respects device access grants)
- Returns paginated results with opaque cursors
- Directly queries PostgreSQL (no in-memory caching)
- Uses Bincode 2 serialization over TCP with `SO_NODELAY`

## Architecture

```
Hub (port 8080)                Workflow Catalog (port 9001)
     |                                  |
     | RPC call                         |
     |--------------------------------->|
     |  ListWorkflowsRequest            |
     |  { user_id, limit, cursor }      |
     |                                  |
     |                         Query PostgreSQL
     |                                  |
     |<---------------------------------|
     |  ListWorkflowsResponse           |
     |  { items[], next_cursor }        |
```

## Running

### Start the service

```bash
cd services/nagisalake-workflow-catalog
cp config.example.toml config.toml
# Edit config.toml with your database URL
CONFIG_PATH=config.toml cargo run --bin nagisalake-workflow-catalog-server
```

### Test with the example client

```bash
cargo run --example client -- 127.0.0.1:9001 <user-id>
```

## Integration with Hub

The Hub can call the catalog service instead of querying the database directly:

```rust
use nagisalake_rpc::{Client, ClientBuilder};
use nagisalake_workflow_catalog::{ListWorkflowsRequest, ListWorkflowsResponse};

// In Hub startup
let workflow_catalog_client: Client<ListWorkflowsRequest, ListWorkflowsResponse> = 
    ClientBuilder::new()
    .connect("127.0.0.1:9001".parse()?)
    .await?;

// In request handler
let response = workflow_catalog_client
    .call(
        ClientContext::default(),
        ListWorkflowsRequest {
            user_id: auth.user_id.clone(),
            limit: Some(50),
            cursor: None,
        },
    )
    .await?;
```

## Performance

**Expected throughput:** ~150K req/s (single-core Bincode ser/de)

**Latency:**
- P50: ~300µs (localhost TCP)
- P99: ~800µs

**Bottleneck:** PostgreSQL query time, not RPC overhead.

## Future Optimizations

### Phase 1: In-memory cache (if needed)

Add an LRU cache layer:

```rust
pub struct CachedWorkflowCatalog {
    store: Arc<PgStore>,
    cache: Arc<Mutex<LruCache<CacheKey, Arc<Vec<WorkflowMetadata>>>>>,
}
```

**Trigger:** Database becomes saturated with catalog queries.

### Phase 2: Zero-copy codec (if needed)

Replace Bincode with CapnProto for 3-4× throughput:

```toml
[features]
zero-copy = ["capnp", "capnpc"]
```

**Trigger:** CPU profiling shows >30% time in ser/de.

## Design Rationale

### Why separate this service?

1. **Independent scaling**: Catalog queries have different load from job dispatch
2. **Deployment flexibility**: Can run on read-replica database
3. **Failure isolation**: Catalog outage doesn't stop job execution
4. **Clear ownership**: Workflow metadata has distinct lifecycle from sessions

### Why Bincode, not CapnProto?

- Simpler: No schema compiler, works with `serde`
- Debuggable: Human-readable with `bincode::deserialize`
- Fast enough: Not the bottleneck (database is)
- Upgradable: Can add `zero-copy` feature later

### Why TCP, not Unix domain socket?

- Deployment flexibility: Service can run on different machine
- Negligible overhead: Localhost TCP is ~200µs, acceptable for catalog queries
- Can switch to UDS later with same RPC interface

## Testing

```bash
cargo test -p nagisalake-workflow-catalog
```

## Monitoring

Key metrics to track:

- `rpc_requests_total{method="list_workflows"}`
- `rpc_request_duration_seconds{method="list_workflows"}`
- `rpc_errors_total{code="internal"}`
- `postgres_query_duration_seconds{query="workflows_for_user_devices_page"}`

If P99 latency exceeds 5ms, investigate database query plan or add caching.
