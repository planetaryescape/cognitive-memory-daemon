//! IPC protocol types and codec for cognitive-memory-daemon.
//!
//! Public wire contract. Length-delimited JSON over Unix domain socket.
//! `IPC_PROTOCOL_VERSION` is bumped on breaking changes; additive changes are
//! free.
//!
//! See `PROTOCOL.md` (repo root) for the full spec. This crate is the
//! authoritative implementation of that spec.

mod codec;

pub use codec::IpcCodec;
use serde::{Deserialize, Serialize};

/// IPC protocol version. Bumped on breaking changes to request/response/event
/// shapes. Additive changes (new variants, new fields with `#[serde(default)]`)
/// do not bump this constant.
///
/// See `PROTOCOL.md` §8 (versioning) and `AGENTS.md` §3 rule 5.
pub const IPC_PROTOCOL_VERSION: u32 = 1;

/// Validate a client's protocol version against the daemon's. Returns
/// `ProtocolMismatch` on disagreement so the caller can surface a clear
/// error to the client.
///
/// See `PROTOCOL.md` §2 (Connection setup).
pub fn validate_protocol_version(client_version: u32) -> Result<(), ProtocolMismatch> {
    if client_version == IPC_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolMismatch {
            client_version,
            daemon_version: IPC_PROTOCOL_VERSION,
        })
    }
}

/// The version the client claimed and the version the daemon supports.
/// Surfaced in the `Error { kind: "ProtocolMismatch", ... }` response that
/// the daemon sends before closing a mismatched connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error(
    "protocol version mismatch: client sent {client_version}, daemon supports {daemon_version}"
)]
pub struct ProtocolMismatch {
    pub client_version: u32,
    pub daemon_version: u32,
}

/// Top-level IPC envelope. One per message on the wire.
///
/// `id` correlates request and response on a single connection. Clients
/// allocate `id` monotonically per connection starting at 1; the daemon
/// echoes it on responses. Events use `id = 0`.
///
/// `Eq` is deliberately not derived: payloads can carry `f32` (search
/// scores), and `f32: !Eq`. Tests use `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpcMessage {
    pub id: u64,
    pub payload: IpcPayload,
}

/// Variant of message: request, response, or event. Tagged on the wire by
/// the `kind` field, with the variant data carried under `body`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body")]
pub enum IpcPayload {
    Request(Request),
    Response(Response),
    Event(Event),
}

/// Bucketed request type. The bucket is the major axis of the protocol;
/// see `PROTOCOL.md` §4.
///
/// `UnknownBucket` is a forward-compat catch-all: a wire payload with a
/// bucket name this build doesn't know decodes to this variant rather than
/// erroring at the wire-format layer. The handler then returns a typed
/// `InvalidPayload` error to the caller. See `PROTOCOL.md` §8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "bucket")]
pub enum Request {
    Diagnostics(DiagnosticsRequest),
    Memory(MemoryRequest),
    Lifecycle(LifecycleRequest),
    #[serde(other)]
    UnknownBucket,
}

/// Lifecycle-bucket requests. Maps to the SDK adapter's lifecycle helpers
/// plus the Tick + Clear operators the daemon owns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum LifecycleRequest {
    /// Run a maintenance pass (decay materialisation, consolidation candidates).
    Tick(TickArgs),
    /// Find memories below a retention threshold — consolidation candidates.
    FindFading(FindFadingArgs),
    /// Find high-stability memories — core promotion candidates.
    FindStable(FindStableArgs),
    /// Mark memories as superseded by a summary memory.
    MarkSuperseded(MarkSupersededArgs),
    /// Migrate a memory to cold storage.
    MigrateToCold(MigrateToColdArgs),
    /// Restore a cold memory to hot.
    MigrateToHot(MigrateToHotArgs),
    /// Convert a memory to an archival stub.
    ConvertToStub(ConvertToStubArgs),
    /// Update one memory's retention floor.
    UpdateRetention(UpdateRetentionArgs),
    /// Delete all memories under a user_id (the SDK's `clear()`).
    Clear(ClearArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TickArgs {
    #[serde(default)]
    pub synchronous: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindFadingArgs {
    pub user_id: String,
    pub max_retention: f64,
    #[serde(default = "default_find_limit")]
    pub limit: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindStableArgs {
    pub user_id: String,
    pub min_stability: f64,
    pub min_access_count: i64,
    #[serde(default = "default_find_limit")]
    pub limit: i64,
}

fn default_find_limit() -> i64 {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkSupersededArgs {
    pub user_id: String,
    pub ids: Vec<String>,
    pub summary_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateToColdArgs {
    pub user_id: String,
    pub id: String,
    pub cold_since: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateToHotArgs {
    pub user_id: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertToStubArgs {
    pub user_id: String,
    pub id: String,
    pub stub_content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateRetentionArgs {
    pub user_id: String,
    pub id: String,
    pub retention_floor: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearArgs {
    pub user_id: String,
    /// Safety: refuse to run unless the caller acknowledges.
    pub confirm: bool,
}

/// Diagnostics-bucket requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum DiagnosticsRequest {
    Status,
    /// Mint a bearer token for `cm-http` use. The daemon stores a salted
    /// hash; the raw token is returned once and not recoverable.
    MintBridgeToken(MintBridgeTokenArgs),
    /// Per-user tier counts (hot/cold/stub/total).
    Counts(CountsArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountsArgs {
    pub user_id: String,
}

/// Arguments for `Diagnostics::MintBridgeToken`. Tokens are scoped to a
/// `user_id` and a capability set; default TTL is 30 days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintBridgeTokenArgs {
    pub user_id: String,
    pub scope: BridgeScope,
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u64,
}

fn default_ttl() -> u64 {
    2_592_000 // 30 days
}

/// Capability scope for an HTTP bridge token. See ADR 0005.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeScope {
    Read,
    Write,
    Admin,
}

/// Memory-bucket requests. Full v0.1.0 surface — feature parity with the
/// SDK's `MemoryAdapter` interface, plus paper-faithful batch storage with
/// auto-association (paper §3.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum MemoryRequest {
    Store(StoreMemoryArgs),
    /// Store many memories in one call. Memories created together get
    /// bidirectional associations between every pair (paper §3.6:
    /// "memories form bidirectional associations when they are retrieved
    /// together OR created in the same context"). Default initial weight
    /// 0.5; override per call.
    StoreBatch(StoreBatchArgs),
    Search(SearchMemoryArgs),
    Get(GetMemoryArgs),
    GetMany(GetManyMemoryArgs),
    List(ListMemoryArgs),
    Update(UpdateMemoryArgs),
    Delete(DeleteMemoryArgs),
    DeleteMany(DeleteManyMemoryArgs),
    Link(LinkMemoryArgs),
    Unlink(UnlinkMemoryArgs),
    GetLinked(GetLinkedArgs),
    GetLinkedMany(GetLinkedManyArgs),
    /// Search by raw embedding vector — for clients that have already
    /// embedded the query themselves.
    VectorSearch(VectorSearchArgs),
    /// BM25-only search — exposed separately from the hybrid mode in
    /// `Search`.
    SearchLexical(SearchLexicalArgs),
    BatchUpdate(BatchUpdateArgs),
}

/// One memory's worth of args for `Memory::Store` and items in
/// `Memory::StoreBatch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreBatchArgs {
    pub user_id: String,
    pub memories: Vec<BatchMemoryEntry>,
    /// Initial weight for the auto-created bidirectional links between
    /// every pair of memories in this batch. Defaults to 0.5.
    #[serde(default = "default_batch_link_weight")]
    pub initial_link_weight: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchMemoryEntry {
    pub content: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default = "default_memory_type")]
    pub memory_type: String,
    #[serde(default = "default_metadata")]
    pub metadata: String,
}

fn default_category() -> String {
    "semantic".to_string()
}
fn default_memory_type() -> String {
    "fact".to_string()
}
fn default_batch_link_weight() -> f64 {
    0.5
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetMemoryArgs {
    pub user_id: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetManyMemoryArgs {
    pub user_id: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ListMemoryArgs {
    pub user_id: String,
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    #[serde(default)]
    pub memory_types: Option<Vec<String>>,
    #[serde(default)]
    pub min_retention_floor: Option<f64>,
    #[serde(default)]
    pub min_importance: Option<f64>,
    #[serde(default)]
    pub created_after: Option<i64>,
    #[serde(default)]
    pub created_before: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub include_superseded: bool,
    #[serde(default)]
    pub include_cold: bool,
    #[serde(default)]
    pub include_stubs: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateMemoryArgs {
    pub user_id: String,
    pub id: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub metadata: Option<String>,
    #[serde(default)]
    pub retention_floor: Option<f64>,
    #[serde(default)]
    pub importance: Option<f64>,
    #[serde(default)]
    pub stability: Option<f64>,
    #[serde(default)]
    pub valid_until: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteMemoryArgs {
    pub user_id: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteManyMemoryArgs {
    pub user_id: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkMemoryArgs {
    pub user_id: String,
    pub source_id: String,
    pub target_id: String,
    #[serde(default = "default_link_strength")]
    pub strength: f64,
    /// If true, also create the reverse edge (target → source).
    #[serde(default = "default_true")]
    pub bidirectional: bool,
    #[serde(default = "default_link_kind")]
    pub kind: String,
}

fn default_link_strength() -> f64 {
    0.1
}
fn default_link_kind() -> String {
    "explicit".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkMemoryArgs {
    pub user_id: String,
    pub source_id: String,
    pub target_id: String,
    #[serde(default = "default_true")]
    pub bidirectional: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetLinkedArgs {
    pub user_id: String,
    pub source_id: String,
    #[serde(default = "default_min_link_strength")]
    pub min_strength: f64,
}

fn default_min_link_strength() -> f64 {
    0.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetLinkedManyArgs {
    pub user_id: String,
    pub source_ids: Vec<String>,
    #[serde(default = "default_min_link_strength")]
    pub min_strength: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchArgs {
    pub user_id: String,
    pub embedding: Vec<f32>,
    pub embedding_provider: String,
    pub embedding_model: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub deep_recall: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchLexicalArgs {
    pub user_id: String,
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchUpdateArgs {
    pub user_id: String,
    pub updates: Vec<RetentionUpdate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetentionUpdate {
    pub id: String,
    pub retention_floor: f64,
}

/// Inputs for `Memory::Store`. v1 stores one memory per call. Embedding
/// is computed daemon-side using the configured provider; clients do not
/// supply vectors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreMemoryArgs {
    pub user_id: String,
    pub content: String,
    pub category: String,
    pub memory_type: String,
    #[serde(default = "default_metadata")]
    pub metadata: String,
    /// Optional explicit importance in [0.0, 1.0]. When omitted, the
    /// daemon uses its default (currently 0.0). Additive on the wire —
    /// older clients omit the field; older daemons ignore it.
    #[serde(default)]
    pub importance: Option<f64>,
}

fn default_metadata() -> String {
    "{}".to_string()
}

/// Inputs for `Memory::Search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMemoryArgs {
    pub user_id: String,
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub deep_recall: bool,
    /// Opt into hybrid (dense + BM25 fused via RRF) retrieval. Default
    /// is dense-only — additive field, no protocol version bump.
    #[serde(default)]
    pub hybrid: bool,
}

fn default_limit() -> usize {
    10
}

/// Response envelope. `ok: true` carries `data`; `ok: false` carries `error`.
/// See `PROTOCOL.md` §5 and §7.
///
/// Use the [`Response::ok`] and [`Response::err`] constructors to build valid
/// responses; direct construction with mismatched fields is a bug.
///
/// `Eq` is deliberately not derived: `ResponseData::MemorySearchResults`
/// carries `f32` scores and `f32: !Eq`. Tests use `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<ResponseData>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<ResponseError>,
}

impl Response {
    /// Build a successful response carrying typed data.
    pub fn ok(data: ResponseData) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    /// Build a failed response carrying a typed error.
    pub fn err(error: ResponseError) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(error),
        }
    }
}

/// Typed response payload. One variant per request kind.
///
/// `large_enum_variant` is allowed: the largest variant (`MemoryData`,
/// ~296 bytes) is the most common payload. Boxing every variant for a
/// rarely-mismatched size delta would obscure the JSON-derived structure
/// for marginal memory savings.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ResponseData {
    Status(StatusData),
    MemoryStored(MemoryStoredData),
    MemoryStoredBatch(MemoryStoredBatchData),
    MemorySearchResults(MemorySearchResultsData),
    BridgeToken(BridgeTokenData),
    Counts(CountsData),
    Memory(MemoryData),
    Memories(MemoriesData),
    Affected(AffectedData),
    LinkedMemories(LinkedMemoriesData),
    LinkStrength(LinkStrengthData),
    LexicalIds(LexicalIdsData),
    Tick(TickResultData),
}

/// Outcome of `Memory::StoreBatch` — the assigned ids in the order they
/// were submitted plus the count of associations created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryStoredBatchData {
    pub ids: Vec<String>,
    pub associations_created: u64,
}

/// `Diagnostics::Counts` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountsData {
    pub hot: i64,
    pub cold: i64,
    pub stub: i64,
    pub total: i64,
}

/// One memory returned to the caller. The wire shape is the full v6
/// surface — clients can ignore fields they don't use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryData {
    pub id: String,
    pub user_id: String,
    pub content: String,
    pub category: String,
    pub memory_type: String,
    pub created_at: i64,
    pub last_accessed_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub retention_floor: f64,
    pub retrieval_count: i64,
    pub importance: f64,
    pub stability: f64,
    pub is_cold: bool,
    pub cold_since: Option<i64>,
    pub is_superseded: bool,
    pub superseded_by: Option<String>,
    pub is_stub: bool,
    pub stub_content: Option<String>,
    pub metadata: String,
    /// Retention factor R(m) computed at response time using the paper's
    /// power-law formula. Reflects the *current* decayed value, not the
    /// stored `retention_floor`. Always in `[retention_floor, 1.0]`.
    /// Stubs return 0.0; procedural memories return 1.0.
    #[serde(default)]
    pub current_retention: f64,
    /// How many ticks the memory has been at its retention floor.
    /// Increments on each `cm tick` while at-floor; resets to 0 once
    /// retention recovers (e.g. via access). Used for cold-migration
    /// candidacy.
    #[serde(default)]
    pub days_at_floor: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoriesData {
    pub memories: Vec<MemoryData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedData {
    pub affected: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedMemoriesData {
    pub memories: Vec<LinkedMemoryData>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedMemoryData {
    pub memory: MemoryData,
    pub link_strength: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkStrengthData {
    pub strength: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LexicalIdsData {
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickResultData {
    pub completed: bool,
    pub memories_decayed: u64,
}

/// Successful `Diagnostics::MintBridgeToken` response. The raw token is
/// returned once; the daemon stores a salted hash and discards the raw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeTokenData {
    pub token: String,
    pub expires_at_unix: i64,
}

/// Successful `Memory::Store` response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryStoredData {
    pub id: String,
}

/// Successful `Memory::Search` response payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySearchResultsData {
    pub results: Vec<SearchHit>,
}

/// One hit in a search response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub memory_id: String,
    pub content: String,
    pub category: String,
    pub memory_type: String,
    pub score: f32,
}

/// Status response body. See `PROTOCOL.md` §5.3.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusData {
    pub daemon_version: String,
    pub uptime_seconds: u64,
    pub memory_count: u64,
}

/// Typed error returned in `Response::Err`. See `PROTOCOL.md` §7.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseError {
    pub kind: ResponseErrorKind,
    pub message: String,
    pub retriable: bool,
}

/// Closed enum of error kinds. Adding a kind is additive (clients ignore
/// unknown kinds via `Other`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseErrorKind {
    ProtocolMismatch,
    InvalidPayload,
    InvalidQuery,
    NotFound,
    Conflict,
    ProviderError,
    NoLlmConfigured,
    StorageError,
    RateLimited,
    ShuttingDown,
    Internal,
}

/// Server-pushed event. Delivered on connections that have subscribed via
/// `Memory::Subscribe`. See `PROTOCOL.md` §6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Event {
    /// Periodic maintenance pass completed. Phase 0 includes this as a
    /// minimal Event variant; lifecycle/memory events land in later phases.
    TickCompleted {
        memories_decayed: u64,
        consolidations_attempted: u64,
        occurred_at: String,
    },
}
