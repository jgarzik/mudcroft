//! Cluster transport: Raft RPCs and cluster control between `mudd` nodes.
//!
//! One TLS listener per node (`--cluster-listen`). Following the sister
//! project roodb:
//! - raw TCP inside TLS, `TCP_NODELAY`, each frame sent as one write (two
//!   writes caused delayed-ACK stalls longer than the RPC timeout there);
//! - request frame `[kind u8][len u32 BE][JSON body]`, response frame
//!   `[status u8][len u32 BE][body]` (status 0: JSON reply, 1: error text);
//! - one persistent connection per peer, reconnected (and the request sent
//!   once more) when a kept connection turns out to be dead;
//! - every RPC under the timeout openraft passes (`soft_ttl`): openraft
//!   leaves timeouts to the transport;
//! - snapshots on a dedicated connection, streamed in 1 MiB chunks.
//!
//! Security: the server is verified against the cluster CA (certificates
//! carry the name [`SERVER_NAME`]); then the client sends `Hello` with the
//! shared secret, compared in constant time. Nothing else is accepted
//! before that.

use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openraft::alias::{SnapshotMetaOf, SnapshotOf, VoteOf};
use openraft::async_runtime::WatchReceiver;
use openraft::error::{RPCError, ReplicationClosed, StreamingError, Unreachable};
use openraft::network::v2::RaftNetworkV2;
use openraft::network::{RPCOption, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, TransferLeaderRequest,
    TransferLeaderResponse, VoteRequest, VoteResponse,
};
use openraft::{BasicNode, ChangeMembers, OptionalSend};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::types::{NodeId, TypeConfig};
use super::GameRaft;

/// The name every cluster certificate must carry (subjectAltName DNS), so
/// a node is verified whatever address it is reached at
pub const SERVER_NAME: &str = "mudd-cluster";

/// Largest frame accepted (a log batch can carry image data)
const MAX_FRAME: usize = 256 * 1024 * 1024;
/// Snapshot chunk size
const SNAPSHOT_CHUNK: usize = 1024 * 1024;
/// Largest snapshot accepted
const MAX_SNAPSHOT: u64 = 16 * 1024 * 1024 * 1024;
/// A snapshot transfer step that takes longer than this has stalled
const SNAPSHOT_STALL: Duration = Duration::from_secs(60);
/// TLS handshake plus Hello must finish within this
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for cluster control requests (join)
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Hello = 1,
    Vote = 2,
    AppendEntries = 3,
    SnapshotHeader = 4,
    SnapshotChunk = 5,
    Join = 6,
    Status = 7,
    Remove = 8,
    /// Forward an HTTP API request to the primary
    Http = 9,
    /// Open a player's game session on the primary; the connection then
    /// carries only that session's frames
    GameOpen = 10,
    /// A stepping-down leader hands leadership to a chosen voter
    TransferLeader = 11,
}

/// Frame tags on a game-session connection (after GameOpen)
const GAME_TEXT: u8 = 0x20;
/// Sent by the primary when it ends the session on purpose (the player was
/// replaced, refused, ...); a connection that drops without it is a failover
const GAME_END: u8 = 0x21;

/// The end reason from a node that is not (yet) the primary: the gateway
/// retries quietly, without telling the player anything changed
pub const NOT_PRIMARY: &str = "not primary";

impl Kind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Kind::Hello,
            2 => Kind::Vote,
            3 => Kind::AppendEntries,
            4 => Kind::SnapshotHeader,
            5 => Kind::SnapshotChunk,
            6 => Kind::Join,
            7 => Kind::Status,
            8 => Kind::Remove,
            9 => Kind::Http,
            10 => Kind::GameOpen,
            11 => Kind::TransferLeader,
            _ => return None,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Hello {
    secret: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct SnapshotHeader {
    vote: VoteOf<TypeConfig>,
    meta: SnapshotMetaOf<TypeConfig>,
    len: u64,
}

/// A node asking to join: the address peers reach it at
#[derive(Debug, Serialize, Deserialize)]
pub struct JoinRequest {
    pub advertise: String,
}

/// The answer to a join request
#[derive(Debug, Serialize, Deserialize)]
pub enum JoinReply {
    /// Admitted: the node's id. The leader adds it as a learner, then a
    /// voter, once it serves its cluster port.
    Joined { node_id: NodeId },
    /// Ask the leader instead (its address, if known)
    NotLeader { leader: Option<String> },
}

/// One member as `status` reports it
#[derive(Debug, Serialize, Deserialize)]
pub struct MemberStatus {
    pub node_id: NodeId,
    pub addr: String,
    pub voter: bool,
}

/// A node's view of the cluster (`mudd_admin cluster status`)
#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterStatus {
    pub node_id: NodeId,
    pub leader: Option<NodeId>,
    pub members: Vec<MemberStatus>,
    pub last_log_index: Option<u64>,
    pub last_applied_index: Option<u64>,
}

/// Remove a node from the cluster
#[derive(Debug, Serialize, Deserialize)]
pub struct RemoveRequest {
    pub node_id: NodeId,
}

/// The answer to a remove request
#[derive(Debug, Serialize, Deserialize)]
pub enum RemoveReply {
    Removed,
    NotLeader { leader: Option<String> },
    Refused(String),
}

/// An HTTP API request forwarded from a gateway to the primary
#[derive(Debug, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    /// Path and query
    pub uri: String,
    /// The headers the API reads (authorization, content-type)
    pub headers: Vec<(String, String)>,
    /// Body, base64
    pub body: String,
}

/// The primary's response to a forwarded request
#[derive(Debug, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Body, base64
    pub body: String,
}

/// Opens a player's session on the primary
#[derive(Debug, Serialize, Deserialize)]
pub struct GameOpen {
    pub universe: String,
    pub token: String,
}

type BoxFuture<T> = std::pin::Pin<Box<dyn Future<Output = T> + Send>>;

/// Serves forwarded HTTP requests (installed by the API layer)
pub type HttpHook = Arc<dyn Fn(HttpRequest) -> BoxFuture<HttpResponse> + Send + Sync>;
/// Serves a forwarded game session until it ends (installed by the API layer)
pub type GameHook = Arc<dyn Fn(GameOpen, GameStream) -> BoxFuture<()> + Send + Sync>;

/// A game session's frames over a cluster connection, from either end.
/// Received text arrives through a channel filled by a reader task, so
/// `recv` is cancel-safe (usable in `select!`).
pub struct GameStream {
    incoming: tokio::sync::mpsc::Receiver<GameFrame>,
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    /// Holds the read half: aborted on drop, so dropping the stream closes
    /// the connection (the other end then sees it end)
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for GameStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// What the other end sent
#[derive(Debug)]
pub enum GameFrame {
    Text(String),
    /// Ended on purpose (with the reason)
    End(String),
}

impl GameStream {
    fn new<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: S) -> Self {
        let (mut reader, writer) = tokio::io::split(stream);
        let (tx, incoming) = tokio::sync::mpsc::channel(64);
        let reader = tokio::spawn(async move {
            loop {
                let frame = match read_frame(&mut reader, MAX_FRAME).await {
                    Ok((GAME_TEXT, body)) => {
                        GameFrame::Text(String::from_utf8_lossy(&body).into_owned())
                    }
                    Ok((GAME_END, body)) => {
                        let _ = tx
                            .send(GameFrame::End(String::from_utf8_lossy(&body).into_owned()))
                            .await;
                        return;
                    }
                    Ok(_) | Err(_) => return,
                };
                if tx.send(frame).await.is_err() {
                    return;
                }
            }
        });
        Self {
            incoming,
            writer: Box::new(writer),
            reader,
        }
    }

    /// The next frame; None when the connection dropped
    pub async fn recv(&mut self) -> Option<GameFrame> {
        self.incoming.recv().await
    }

    /// Send text; false if the connection is gone
    pub async fn send(&mut self, text: &str) -> bool {
        write_frame(&mut self.writer, GAME_TEXT, text.as_bytes())
            .await
            .is_ok()
    }

    /// End the session on purpose
    pub async fn end(&mut self, reason: &str) {
        let _ = write_frame(&mut self.writer, GAME_END, reason.as_bytes()).await;
    }
}

/// Open a player's game session on the node at `addr` (the primary)
pub async fn open_game_session(
    addr: &str,
    security: &ClusterSecurity,
    open: &GameOpen,
) -> io::Result<GameStream> {
    let mut stream = connect(addr, security).await?;
    write_frame(&mut stream, Kind::GameOpen as u8, &encode(open)?).await?;
    Ok(GameStream::new(stream))
}

// ---------------------------------------------------------------------------
// Security
// ---------------------------------------------------------------------------

/// The cluster's TLS configuration and shared secret, supplied by the
/// operator (see admin.md)
#[derive(Debug, Clone)]
pub struct ClusterFiles {
    /// This node's certificate (PEM), signed by the cluster CA, with
    /// subjectAltName DNS:mudd-cluster
    pub cert: PathBuf,
    /// Its private key (PEM)
    pub key: PathBuf,
    /// The cluster CA certificate (PEM)
    pub ca: PathBuf,
    /// The shared secret (at least 16 bytes; surrounding whitespace ignored)
    pub secret: PathBuf,
}

/// Loaded TLS configurations and the secret
pub struct ClusterSecurity {
    server: Arc<rustls::ServerConfig>,
    client: Arc<rustls::ClientConfig>,
    secret: Vec<u8>,
}

impl std::fmt::Debug for ClusterSecurity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClusterSecurity { .. }")
    }
}

fn read_pem_certs(path: &Path) -> anyhow::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;
    let certs = rustls::pki_types::CertificateDer::pem_file_iter(path)
        .map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))?;
    if certs.is_empty() {
        anyhow::bail!("{}: no certificate found", path.display());
    }
    Ok(certs)
}

fn read_secret(path: &Path) -> anyhow::Result<Vec<u8>> {
    let secret = std::fs::read(path).map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))?;
    let secret = secret.trim_ascii().to_vec();
    if secret.len() < 16 {
        anyhow::bail!(
            "{}: the cluster secret must be at least 16 bytes",
            path.display()
        );
    }
    Ok(secret)
}

fn client_config(
    provider: Arc<rustls::crypto::CryptoProvider>,
    ca: &Path,
) -> anyhow::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for ca in read_pem_certs(ca)? {
        roots.add(ca)?;
    }
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

impl ClusterSecurity {
    /// For a tool that only connects (mudd_admin): the CA and the secret,
    /// no certificate of its own; it cannot serve
    pub fn client_only(ca: &Path, secret: &Path) -> anyhow::Result<Self> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let client = client_config(provider.clone(), ca)?;
        // A server config nobody will use (no certificate to serve with)
        let server = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
        Ok(Self {
            server: Arc::new(server),
            client: Arc::new(client),
            secret: read_secret(secret)?,
        })
    }

    pub fn load(files: &ClusterFiles) -> anyhow::Result<Self> {
        use rustls::pki_types::pem::PemObject;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let certs = read_pem_certs(&files.cert)?;
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&files.key)
            .map_err(|e| anyhow::anyhow!("{}: {}", files.key.display(), e))?;
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        let client = client_config(provider, &files.ca)?;
        Ok(Self {
            server: Arc::new(server),
            client: Arc::new(client),
            secret: read_secret(&files.secret)?,
        })
    }
}

/// Constant-time comparison (no early exit on the first difference)
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (i, x) in a.iter().enumerate() {
        diff |= (*x ^ b.get(i).copied().unwrap_or(!*x)) as usize;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Write one frame in a single write
async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, tag: u8, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.push(tag);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(body);
    w.write_all(&buf).await?;
    w.flush().await
}

/// Read one frame (tag, body), refusing bodies over `limit`
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, limit: usize) -> io::Result<(u8, Vec<u8>)> {
    let tag = r.read_u8().await?;
    let len = r.read_u32().await? as usize;
    if len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {} bytes exceeds {}", len, limit),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok((tag, body))
}

/// Bodies are JSON: log entries carry SQL parameters as `serde_json::Value`,
/// which a non-self-describing format (bincode) cannot decode. Snapshot
/// data travels as raw chunk frames.
fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(io::Error::other)
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> io::Result<T> {
    serde_json::from_slice(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read a response frame: the reply, or the peer's error text
async fn read_reply<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
) -> io::Result<Result<T, String>> {
    let (status, body) = read_frame(r, MAX_FRAME).await?;
    match status {
        0 => Ok(Ok(decode(&body)?)),
        _ => Ok(Err(String::from_utf8_lossy(&body).into_owned())),
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

type ClientStream = tokio_rustls::client::TlsStream<TcpStream>;

/// Open an authenticated connection: TCP, TLS (server verified against the
/// CA as SERVER_NAME), then Hello with the secret
async fn connect(addr: &str, security: &ClusterSecurity) -> io::Result<ClientStream> {
    let attempt = async {
        let tcp = TcpStream::connect(addr).await?;
        tcp.set_nodelay(true)?;
        let name =
            rustls::pki_types::ServerName::try_from(SERVER_NAME).map_err(io::Error::other)?;
        let mut tls = tokio_rustls::TlsConnector::from(security.client.clone())
            .connect(name, tcp)
            .await?;
        write_frame(
            &mut tls,
            Kind::Hello as u8,
            &encode(&Hello {
                secret: security.secret.clone(),
            })?,
        )
        .await?;
        match read_reply::<_, ()>(&mut tls).await? {
            Ok(()) => Ok(tls),
            Err(e) => Err(io::Error::new(io::ErrorKind::PermissionDenied, e)),
        }
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, attempt)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))?
}

/// A peer's address and the connection kept to it between requests
pub struct PeerClient {
    addr: String,
    security: Arc<ClusterSecurity>,
    /// Taken out for the length of a request and put back only once it has
    /// been answered, so a failed or abandoned request leaves none behind
    stream: Mutex<Option<ClientStream>>,
}

impl PeerClient {
    pub fn new(addr: impl Into<String>, security: Arc<ClusterSecurity>) -> Self {
        Self {
            addr: addr.into(),
            security,
            stream: Mutex::new(None),
        }
    }

    async fn exchange<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        kind: Kind,
        request: &Req,
    ) -> io::Result<Result<Resp, String>> {
        let body = encode(request)?;
        let kept = self.stream.lock().await.take();
        let reused = kept.is_some();
        let mut stream = match kept {
            Some(s) => s,
            None => connect(&self.addr, &self.security).await?,
        };
        let result = async {
            write_frame(&mut stream, kind as u8, &body).await?;
            read_reply(&mut stream).await
        }
        .await;
        let (stream, result) = match result {
            // A kept connection may have died meanwhile (the peer
            // restarted): try once more on a new one
            Err(_) if reused => {
                let mut fresh = connect(&self.addr, &self.security).await?;
                write_frame(&mut fresh, kind as u8, &body).await?;
                let r = read_reply(&mut fresh).await?;
                (fresh, r)
            }
            Err(e) => return Err(e),
            Ok(r) => (stream, r),
        };
        *self.stream.lock().await = Some(stream);
        Ok(result)
    }

    /// Send one request and read its reply within `limit`
    async fn request<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        kind: Kind,
        request: &Req,
        limit: Duration,
    ) -> Result<Resp, String> {
        match tokio::time::timeout(limit, self.exchange(kind, request)).await {
            Ok(Ok(Ok(reply))) => Ok(reply),
            Ok(Ok(Err(remote))) => Err(format!("{} ({:?}): {}", self.addr, kind, remote)),
            Ok(Err(e)) => Err(format!("{} ({:?}): {}", self.addr, kind, e)),
            Err(_) => Err(format!(
                "{} ({:?}): no reply within {:?}",
                self.addr, kind, limit
            )),
        }
    }

    /// Ask to join the cluster
    pub async fn join(&self, advertise: &str) -> Result<JoinReply, String> {
        self.request(
            Kind::Join,
            &JoinRequest {
                advertise: advertise.to_string(),
            },
            CONTROL_TIMEOUT,
        )
        .await
    }

    /// Forward an HTTP request
    pub async fn http(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        // A script run can take a while
        self.request(Kind::Http, request, Duration::from_secs(120))
            .await
    }

    /// This node's view of the cluster
    pub async fn status(&self) -> Result<ClusterStatus, String> {
        self.request(Kind::Status, &(), CONTROL_TIMEOUT).await
    }

    /// Ask to remove a node
    pub async fn remove(&self, node_id: NodeId) -> Result<RemoveReply, String> {
        self.request(Kind::Remove, &RemoveRequest { node_id }, CONTROL_TIMEOUT)
            .await
    }

    /// Stream a snapshot on a dedicated connection
    async fn send_snapshot(
        &self,
        vote: VoteOf<TypeConfig>,
        meta: SnapshotMetaOf<TypeConfig>,
        data: Vec<u8>,
    ) -> io::Result<Result<SnapshotResponse<TypeConfig>, String>> {
        let stalled = || io::Error::new(io::ErrorKind::TimedOut, "snapshot transfer stalled");
        let mut stream = connect(&self.addr, &self.security).await?;
        let header = encode(&SnapshotHeader {
            vote,
            meta,
            len: data.len() as u64,
        })?;
        tokio::time::timeout(
            SNAPSHOT_STALL,
            write_frame(&mut stream, Kind::SnapshotHeader as u8, &header),
        )
        .await
        .map_err(|_| stalled())??;
        for chunk in data.chunks(SNAPSHOT_CHUNK) {
            tokio::time::timeout(
                SNAPSHOT_STALL,
                write_frame(&mut stream, Kind::SnapshotChunk as u8, chunk),
            )
            .await
            .map_err(|_| stalled())??;
        }
        tokio::time::timeout(
            SNAPSHOT_STALL,
            write_frame(&mut stream, Kind::SnapshotChunk as u8, &[]),
        )
        .await
        .map_err(|_| stalled())??;
        // Installing takes a while on the receiver
        tokio::time::timeout(SNAPSHOT_STALL * 10, read_reply(&mut stream))
            .await
            .map_err(|_| stalled())?
    }
}

// ---------------------------------------------------------------------------
// openraft network
// ---------------------------------------------------------------------------

/// Creates a client per peer, at the address in the replicated membership
pub struct ClusterNetwork {
    security: Option<Arc<ClusterSecurity>>,
}

impl ClusterNetwork {
    /// Without security (single-node mode) every peer is unreachable
    pub fn new(security: Option<Arc<ClusterSecurity>>) -> Self {
        Self { security }
    }
}

impl RaftNetworkFactory<TypeConfig> for ClusterNetwork {
    type Network = PeerNetwork;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Self::Network {
        PeerNetwork {
            target,
            client: self
                .security
                .clone()
                .filter(|_| !node.addr.is_empty())
                .map(|security| PeerClient::new(node.addr.clone(), security)),
        }
    }
}

/// The network to one peer
pub struct PeerNetwork {
    target: NodeId,
    client: Option<PeerClient>,
}

impl PeerNetwork {
    fn client(&self) -> Result<&PeerClient, Unreachable<TypeConfig>> {
        self.client.as_ref().ok_or_else(|| {
            Unreachable::from_string(format!(
                "node {}: no cluster transport or address",
                self.target
            ))
        })
    }
}

impl RaftNetworkV2<TypeConfig> for PeerNetwork {
    type SnapshotData = Cursor<Vec<u8>>;

    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<openraft::raft::AppendResult<TypeConfig>, RPCError<TypeConfig>> {
        // The reply says only success, conflict or a higher vote; the log
        // positions it refers to are the ones sent
        let prev_log_id = rpc.prev_log_id;
        let last_log_id = rpc.last_log_id();
        let client = self.client().map_err(RPCError::Unreachable)?;
        let response: AppendEntriesResponse<TypeConfig> = client
            .request(Kind::AppendEntries, &rpc, option.soft_ttl())
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::from_string(e)))?;
        Ok(response.into_append_result(prev_log_id, last_log_id))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        let client = self.client().map_err(RPCError::Unreachable)?;
        client
            .request(Kind::Vote, &rpc, option.soft_ttl())
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::from_string(e)))
    }

    async fn transfer_leader(
        &mut self,
        rpc: TransferLeaderRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<TransferLeaderResponse<TypeConfig>, RPCError<TypeConfig>> {
        let client = self.client().map_err(RPCError::Unreachable)?;
        client
            .request(Kind::TransferLeader, &rpc, option.soft_ttl())
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::from_string(e)))
    }

    async fn full_snapshot(
        &mut self,
        vote: VoteOf<TypeConfig>,
        snapshot: SnapshotOf<TypeConfig, Self::SnapshotData>,
        cancel: impl Future<Output = ReplicationClosed> + OptionalSend + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<TypeConfig>, StreamingError<TypeConfig>> {
        let client = self.client().map_err(StreamingError::Unreachable)?;
        let send = client.send_snapshot(vote, snapshot.meta, snapshot.snapshot.into_inner());
        tokio::select! {
            closed = cancel => Err(StreamingError::Closed(closed)),
            result = send => match result {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(remote)) => Err(StreamingError::Unreachable(Unreachable::from_string(remote))),
                Err(e) => Err(StreamingError::Unreachable(Unreachable::new(&e))),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// What the cluster port serves
pub struct ClusterService {
    raft: GameRaft,
    /// This node's database (reads: cluster_nodes)
    pool: sqlx::SqlitePool,
    /// Installed by the API layer once its router exists
    http: std::sync::OnceLock<HttpHook>,
    game: std::sync::OnceLock<GameHook>,
    /// One snapshot received at a time
    receiving_snapshot: Mutex<()>,
    /// Joins are admitted one at a time; ids handed out but not yet members
    joining: Mutex<HashMap<String, NodeId>>,
}

impl ClusterService {
    pub fn new(raft: GameRaft, pool: sqlx::SqlitePool) -> Arc<Self> {
        Arc::new(Self {
            raft,
            pool,
            http: std::sync::OnceLock::new(),
            game: std::sync::OnceLock::new(),
            receiving_snapshot: Mutex::new(()),
            joining: Mutex::new(HashMap::new()),
        })
    }

    /// Install the handlers for forwarded HTTP requests and game sessions
    pub fn set_hooks(&self, http: HttpHook, game: GameHook) {
        let _ = self.http.set(http);
        let _ = self.game.set(game);
    }

    /// The leader's cluster address, if known and not this node
    pub fn leader_addr(&self) -> Option<String> {
        self.not_leader().flatten()
    }

    fn status(&self) -> ClusterStatus {
        let m = self.raft.metrics().borrow_watched().clone();
        let membership = m.membership_config.membership();
        let voters: std::collections::BTreeSet<NodeId> = membership.voter_ids().collect();
        ClusterStatus {
            node_id: m.id,
            leader: m.current_leader,
            members: membership
                .nodes()
                .map(|(id, node)| MemberStatus {
                    node_id: *id,
                    addr: node.addr.clone(),
                    voter: voters.contains(id),
                })
                .collect(),
            last_log_index: m.last_log_index,
            last_applied_index: m.last_applied.map(|l| l.index),
        }
    }

    /// The leader's address if this node is not the leader
    fn not_leader(&self) -> Option<Option<String>> {
        let m = self.raft.metrics().borrow_watched().clone();
        // The server state too: a leader that steps down can still name
        // itself as current_leader
        if m.current_leader == Some(m.id) && m.state == openraft::ServerState::Leader {
            return None;
        }
        Some(m.current_leader.and_then(|id| {
            m.membership_config
                .membership()
                .get_node(&id)
                .map(|n| n.addr.clone())
                .filter(|a| !a.is_empty())
        }))
    }

    async fn handle_remove(&self, request: RemoveRequest) -> RemoveReply {
        if let Some(leader) = self.not_leader() {
            return RemoveReply::NotLeader { leader };
        }
        let id = request.node_id;
        let membership = self
            .raft
            .metrics()
            .borrow_watched()
            .membership_config
            .clone();
        let membership = membership.membership();
        if membership.get_node(&id).is_none() {
            return RemoveReply::Refused(format!("node {} is not a member", id));
        }
        let voters: std::collections::BTreeSet<NodeId> = membership.voter_ids().collect();
        if voters.contains(&id) {
            if voters.len() == 1 {
                return RemoveReply::Refused("cannot remove the last voter".to_string());
            }
            if let Err(e) = self
                .raft
                .change_membership(ChangeMembers::RemoveVoters([id].into()), false)
                .await
            {
                return RemoveReply::Refused(e.to_string());
            }
        }
        match self
            .raft
            .change_membership(ChangeMembers::RemoveNodes([id].into()), false)
            .await
        {
            Ok(_) => {
                info!("cluster: node {} removed", id);
                RemoveReply::Removed
            }
            Err(e) => RemoveReply::Refused(e.to_string()),
        }
    }

    async fn handle_join(self: &Arc<Self>, request: JoinRequest) -> JoinReply {
        if let Some(leader) = self.not_leader() {
            return JoinReply::NotLeader { leader };
        }
        let membership = self
            .raft
            .metrics()
            .borrow_watched()
            .membership_config
            .clone();
        if request.advertise.is_empty() || request.advertise.starts_with("0.0.0.0") {
            // Peers could not reach it; answer as if not admitted
            return JoinReply::NotLeader { leader: None };
        }

        let mut joining = self.joining.lock().await;
        // The same node asking again (a retry) gets the same id
        if let Some((id, _)) = membership
            .membership()
            .nodes()
            .find(|(_, n)| n.addr == request.advertise)
        {
            return JoinReply::Joined { node_id: *id };
        }
        let node_id = match joining.get(&request.advertise) {
            Some(id) => *id,
            None => {
                // Above every id ever handed out (cluster_nodes), every
                // member and every join in progress: never reused
                let recorded = match self.highest_recorded_id().await {
                    Ok(id) => id,
                    Err(e) => {
                        warn!("cluster: reading cluster_nodes failed: {}", e);
                        return JoinReply::NotLeader { leader: None };
                    }
                };
                let highest = membership
                    .membership()
                    .nodes()
                    .map(|(id, _)| *id)
                    .chain(joining.values().copied())
                    .chain(std::iter::once(recorded))
                    .max()
                    .unwrap_or(0);
                let id = highest + 1;
                // Recorded through Raft before the answer, so even after a
                // failover nobody hands the id out again
                let record = super::Request::batch(vec![(
                    "INSERT INTO cluster_nodes (node_id, addr, added_at) VALUES (?, ?, ?)"
                        .to_string(),
                    vec![
                        serde_json::json!(id),
                        serde_json::json!(request.advertise),
                        serde_json::json!(chrono::Utc::now().to_rfc3339()),
                    ],
                )]);
                match self.raft.client_write(record).await {
                    Ok(r) if r.data.success => id,
                    Ok(r) => {
                        warn!("cluster: recording node {} failed: {:?}", id, r.data.error);
                        return JoinReply::NotLeader { leader: None };
                    }
                    Err(e) => {
                        warn!("cluster: recording node {} failed: {}", id, e);
                        return JoinReply::NotLeader { leader: None };
                    }
                }
            }
        };
        joining.insert(request.advertise.clone(), node_id);
        drop(joining);

        info!(
            "cluster: admitting node {} at {}",
            node_id, request.advertise
        );
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(e) = service.admit(node_id, request.advertise.clone()).await {
                warn!("cluster: admitting node {} failed: {}", node_id, e);
            }
            service.joining.lock().await.remove(&request.advertise);
        });
        JoinReply::Joined { node_id }
    }

    /// The highest node id ever recorded (0 if none)
    async fn highest_recorded_id(&self) -> anyhow::Result<NodeId> {
        let highest: Option<i64> = sqlx::query_scalar("SELECT MAX(node_id) FROM cluster_nodes")
            .fetch_one(&self.pool)
            .await?;
        Ok(highest.unwrap_or(0).max(0) as NodeId)
    }

    /// Bring a new node in: snapshot and purge (node 1's initial data was
    /// written before the log existed, so a newcomer must start from a
    /// snapshot), then learner, then voter
    async fn admit(&self, node_id: NodeId, addr: String) -> anyhow::Result<()> {
        let raft = &self.raft;
        let applied = raft.metrics().borrow_watched().last_applied;
        raft.trigger().snapshot().await?;
        let mut metrics = raft.metrics();
        let snapshot = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let snapshot = metrics.borrow_watched().snapshot;
                if snapshot.is_some() && snapshot >= applied {
                    return snapshot;
                }
                if metrics.changed().await.is_err() {
                    return None;
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("snapshot not built in time"))?;
        if let Some(snapshot) = snapshot {
            raft.trigger().purge_log(snapshot.index).await?;
        }
        enable_cluster_timers(raft);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            match raft
                .add_learner(node_id, BasicNode::new(addr.clone()), true)
                .await
            {
                Ok(_) => break,
                Err(e) if tokio::time::Instant::now() < deadline => {
                    debug!("cluster: add_learner({}) not yet: {}", node_id, e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
        loop {
            match raft
                .change_membership(ChangeMembers::AddVoterIds([node_id].into()), false)
                .await
            {
                Ok(_) => break,
                Err(e) if tokio::time::Instant::now() < deadline => {
                    debug!("cluster: promoting {} not yet: {}", node_id, e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
        info!("cluster: node {} is a voter", node_id);
        Ok(())
    }

    /// Answer one request frame
    async fn handle<S: AsyncRead + AsyncWrite + Unpin>(
        self: &Arc<Self>,
        stream: &mut S,
        kind: Kind,
        body: Vec<u8>,
    ) -> io::Result<()> {
        let reply: Result<Vec<u8>, String> = match kind {
            Kind::Vote => match decode::<VoteRequest<TypeConfig>>(&body) {
                Ok(rpc) => match self.raft.vote(rpc).await {
                    Ok(r) => encode(&r).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            },
            Kind::AppendEntries => match decode::<AppendEntriesRequest<TypeConfig>>(&body) {
                Ok(rpc) => match self.raft.append_entries(rpc).await {
                    Ok(r) => {
                        let r: AppendEntriesResponse<TypeConfig> = r.into();
                        encode(&r).map_err(|e| e.to_string())
                    }
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            },
            Kind::SnapshotHeader => self.receive_snapshot(stream, &body).await?,
            Kind::Join => match decode::<JoinRequest>(&body) {
                Ok(request) => encode(&self.handle_join(request).await).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            },
            Kind::Status => encode(&self.status()).map_err(|e| e.to_string()),
            Kind::Remove => match decode::<RemoveRequest>(&body) {
                Ok(request) => {
                    encode(&self.handle_remove(request).await).map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
            },
            Kind::TransferLeader => match decode::<TransferLeaderRequest<TypeConfig>>(&body) {
                Ok(rpc) => match self.raft.handle_transfer_leader(rpc).await {
                    Ok(r) => encode(&r).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            },
            Kind::Http => match (decode::<HttpRequest>(&body), self.http.get()) {
                (Ok(request), Some(hook)) => {
                    encode(&hook(request).await).map_err(|e| e.to_string())
                }
                (Err(e), _) => Err(e.to_string()),
                (_, None) => Err("this node is not serving yet".to_string()),
            },
            Kind::Hello | Kind::SnapshotChunk | Kind::GameOpen => {
                Err(format!("unexpected {:?}", kind))
            }
        };
        match reply {
            Ok(body) => write_frame(stream, 0, &body).await,
            Err(e) => write_frame(stream, 1, e.as_bytes()).await,
        }
    }

    async fn receive_snapshot<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        stream: &mut S,
        header: &[u8],
    ) -> io::Result<Result<Vec<u8>, String>> {
        let header: SnapshotHeader = decode(header)?;
        if header.len > MAX_SNAPSHOT {
            return Ok(Err(format!(
                "snapshot of {} bytes is too large",
                header.len
            )));
        }
        let Ok(_one) = self.receiving_snapshot.try_lock() else {
            return Ok(Err("already receiving a snapshot".to_string()));
        };
        let mut data = Vec::with_capacity(header.len.min(64 * 1024 * 1024) as usize);
        loop {
            let (tag, chunk) =
                tokio::time::timeout(SNAPSHOT_STALL, read_frame(stream, SNAPSHOT_CHUNK))
                    .await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "snapshot stalled"))??;
            if tag != Kind::SnapshotChunk as u8 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected a snapshot chunk",
                ));
            }
            if chunk.is_empty() {
                break;
            }
            data.extend_from_slice(&chunk);
            if data.len() as u64 > header.len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "snapshot longer than announced",
                ));
            }
        }
        if data.len() as u64 != header.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot shorter than announced",
            ));
        }
        let snapshot = SnapshotOf::<TypeConfig, Cursor<Vec<u8>>> {
            meta: header.meta,
            snapshot: Cursor::new(data),
        };
        Ok(
            match self.raft.install_full_snapshot(header.vote, snapshot).await {
                Ok(r) => encode(&r).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            },
        )
    }
}

/// Serve the cluster port until the listener fails
pub async fn serve(
    listener: TcpListener,
    security: Arc<ClusterSecurity>,
    service: Arc<ClusterService>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(security.server.clone());
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                warn!("cluster: accept failed: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let security = security.clone();
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_connection(tcp, acceptor, &security, &service).await {
                debug!("cluster: connection from {} closed: {}", peer, e);
            }
        });
    }
}

async fn serve_connection(
    tcp: TcpStream,
    acceptor: tokio_rustls::TlsAcceptor,
    security: &ClusterSecurity,
    service: &Arc<ClusterService>,
) -> io::Result<()> {
    tcp.set_nodelay(true)?;
    // Handshake and Hello within a time limit; nothing else before Hello
    let mut stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut stream = acceptor.accept(tcp).await?;
        let (tag, body) = read_frame(&mut stream, 4096).await?;
        let hello: Option<Hello> = (tag == Kind::Hello as u8)
            .then(|| decode(&body).ok())
            .flatten();
        match hello {
            Some(hello) if same_secret(&hello.secret, &security.secret) => {
                write_frame(&mut stream, 0, &encode(&())?).await?;
                Ok(stream)
            }
            _ => {
                let _ = write_frame(&mut stream, 1, b"cluster authentication failed").await;
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "bad hello"))
            }
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))??;
    loop {
        let (tag, body) = read_frame(&mut stream, MAX_FRAME).await?;
        let Some(kind) = Kind::from_u8(tag) else {
            write_frame(&mut stream, 1, b"unknown request").await?;
            continue;
        };
        if kind == Kind::GameOpen {
            // The connection now belongs to this game session
            let open: GameOpen = decode(&body)?;
            let Some(hook) = service.game.get().cloned() else {
                let _ = write_frame(&mut stream, GAME_END, b"this node is not serving yet").await;
                return Ok(());
            };
            hook(open, GameStream::new(stream)).await;
            return Ok(());
        }
        service.handle(&mut stream, kind, body).await?;
    }
}

/// Turn on ticks, heartbeats and elections (off while the cluster has a
/// single member)
pub fn enable_cluster_timers(raft: &GameRaft) {
    let config = raft.runtime_config();
    config.tick(true);
    config.heartbeat(true);
    config.elect(true);
}

/// Whether a member this node can reach (with a leader) reports a
/// membership without `node_id`: the node was removed while it was away.
/// Best effort: unreachable peers say nothing.
pub async fn removed_by_peers(
    node_id: NodeId,
    peers: &[String],
    security: Arc<ClusterSecurity>,
) -> bool {
    for peer in peers {
        let client = PeerClient::new(peer.clone(), security.clone());
        let status = tokio::time::timeout(Duration::from_secs(5), client.status()).await;
        if let Ok(Ok(status)) = status {
            if status.leader.is_some() {
                return !status.members.iter().any(|m| m.node_id == node_id);
            }
        }
    }
    false
}

/// Join a cluster through any of `seeds`: follows "not the leader" answers
/// to the leader and retries until admitted or `limit` passes
pub async fn join_cluster(
    seeds: &[String],
    advertise: &str,
    security: Arc<ClusterSecurity>,
    limit: Duration,
) -> anyhow::Result<NodeId> {
    let deadline = tokio::time::Instant::now() + limit;
    let mut targets: Vec<String> = seeds.to_vec();
    loop {
        let mut next: Vec<String> = Vec::new();
        for target in &targets {
            let client = PeerClient::new(target.clone(), security.clone());
            match client.join(advertise).await {
                Ok(JoinReply::Joined { node_id }) => {
                    info!("cluster: joined via {} as node {}", target, node_id);
                    return Ok(node_id);
                }
                Ok(JoinReply::NotLeader {
                    leader: Some(leader),
                }) => next.push(leader),
                Ok(JoinReply::NotLeader { leader: None }) => {}
                Err(e) => {
                    // A wrong secret or certificate does not heal with time
                    if e.contains("authentication") {
                        anyhow::bail!("cluster refused this node: {}", e);
                    }
                    if e.contains("certificate") {
                        anyhow::bail!(
                            "cannot verify the cluster's certificate (another CA?): {}",
                            e
                        );
                    }
                    debug!("cluster: join via {}: {}", target, e);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("could not join the cluster via {:?}", seeds);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        next.extend(seeds.iter().cloned());
        next.dedup();
        targets = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_secret() {
        assert!(same_secret(b"0123456789abcdef", b"0123456789abcdef"));
        assert!(!same_secret(b"0123456789abcdef", b"0123456789abcdeX"));
        assert!(!same_secret(b"0123456789abcdef", b"0123456789abcde"));
        assert!(!same_secret(b"", b"x"));
    }

    #[tokio::test]
    async fn test_frames_round_trip_and_limit() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        write_frame(&mut a, 7, b"hello").await.unwrap();
        let (tag, body) = read_frame(&mut b, 16).await.unwrap();
        assert_eq!((tag, body.as_slice()), (7, &b"hello"[..]));
        write_frame(&mut a, 7, &[0u8; 32]).await.unwrap();
        assert!(read_frame(&mut b, 16).await.is_err(), "over the limit");
    }
}
