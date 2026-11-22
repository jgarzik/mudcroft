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
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
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
}

impl Kind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Kind::Hello,
            2 => Kind::Vote,
            3 => Kind::AppendEntries,
            4 => Kind::SnapshotHeader,
            5 => Kind::SnapshotChunk,
            6 => Kind::Join,
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

impl ClusterSecurity {
    pub fn load(files: &ClusterFiles) -> anyhow::Result<Self> {
        use rustls::pki_types::pem::PemObject;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let certs = read_pem_certs(&files.cert)?;
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&files.key)
            .map_err(|e| anyhow::anyhow!("{}: {}", files.key.display(), e))?;
        let mut roots = rustls::RootCertStore::empty();
        for ca in read_pem_certs(&files.ca)? {
            roots.add(ca)?;
        }
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let secret = std::fs::read(&files.secret)
            .map_err(|e| anyhow::anyhow!("{}: {}", files.secret.display(), e))?;
        let secret = secret.trim_ascii().to_vec();
        if secret.len() < 16 {
            anyhow::bail!(
                "{}: the cluster secret must be at least 16 bytes",
                files.secret.display()
            );
        }
        Ok(Self {
            server: Arc::new(server),
            client: Arc::new(client),
            secret,
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
    /// One snapshot received at a time
    receiving_snapshot: Mutex<()>,
    /// Joins are admitted one at a time; ids handed out but not yet members
    joining: Mutex<HashMap<String, NodeId>>,
}

impl ClusterService {
    pub fn new(raft: GameRaft) -> Arc<Self> {
        Arc::new(Self {
            raft,
            receiving_snapshot: Mutex::new(()),
            joining: Mutex::new(HashMap::new()),
        })
    }

    async fn handle_join(self: &Arc<Self>, request: JoinRequest) -> JoinReply {
        let (leader, membership) = {
            let metrics = self.raft.metrics().borrow_watched().clone();
            (metrics.current_leader, metrics.membership_config)
        };
        let me = self.raft.metrics().borrow_watched().id;
        if leader != Some(me) {
            let leader = leader.and_then(|id| {
                membership
                    .membership()
                    .get_node(&id)
                    .map(|n| n.addr.clone())
                    .filter(|a| !a.is_empty())
            });
            return JoinReply::NotLeader { leader };
        }
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
                let highest = membership
                    .membership()
                    .nodes()
                    .map(|(id, _)| *id)
                    .chain(joining.values().copied())
                    .max()
                    .unwrap_or(0);
                highest + 1
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
            Kind::Hello | Kind::SnapshotChunk => Err(format!("unexpected {:?}", kind)),
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
                    if e.contains("authentication") {
                        anyhow::bail!("cluster refused this node: {}", e);
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
