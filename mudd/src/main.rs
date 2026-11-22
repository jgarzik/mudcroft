//! mudd - Mudcroft server daemon

use std::net::SocketAddr;

use anyhow::Result;
use clap::Parser;
use mudd::raft::ClusterFiles;
use mudd::{ClusterConfig, Config, Server};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Mudcroft server daemon
#[derive(Parser, Debug)]
#[command(name = "mudd", version, about)]
struct Args {
    /// Address to bind to
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    /// Database file path (required - must be pre-initialized with mudd_init)
    #[arg(short, long)]
    database: String,

    /// Run as a cluster member, reachable by peers at this host:port (the
    /// cluster port). Requires the --cluster-* files. Omit for a single node.
    #[arg(long, requires_all = ["cluster_cert", "cluster_key", "cluster_ca", "cluster_secret_file"])]
    advertise: Option<String>,

    /// Address the cluster port listens on [default: 0.0.0.0 and the
    /// --advertise port]
    #[arg(long, requires = "advertise")]
    cluster_listen: Option<SocketAddr>,

    /// For a new node (database from `mudd_init --empty`): join the cluster
    /// through these members' cluster ports (comma-separated host:port)
    #[arg(long, requires = "advertise", value_delimiter = ',')]
    join: Vec<String>,

    /// This node's TLS certificate (PEM), signed by the cluster CA, with
    /// subjectAltName DNS:mudd-cluster
    #[arg(long, requires = "advertise")]
    cluster_cert: Option<std::path::PathBuf>,

    /// The certificate's private key (PEM)
    #[arg(long, requires = "advertise")]
    cluster_key: Option<std::path::PathBuf>,

    /// The cluster CA certificate (PEM)
    #[arg(long, requires = "advertise")]
    cluster_ca: Option<std::path::PathBuf>,

    /// File holding the cluster's shared secret (at least 16 bytes)
    #[arg(long, requires = "advertise")]
    cluster_secret_file: Option<std::path::PathBuf>,

    /// Serve the built web client (e.g. client/dist) from this directory on
    /// the same origin as the API. Must contain index.html.
    #[arg(long)]
    static_dir: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mudd=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Parse CLI arguments
    let args = Args::parse();

    // Build config from CLI args
    let cluster = match args.advertise {
        None => None,
        Some(advertise) => {
            let listen = match args.cluster_listen {
                Some(listen) => listen,
                None => {
                    let port = advertise
                        .rsplit_once(':')
                        .and_then(|(_, p)| p.parse::<u16>().ok())
                        .ok_or_else(|| anyhow::anyhow!("--advertise must be host:port"))?;
                    SocketAddr::from(([0, 0, 0, 0], port))
                }
            };
            Some(ClusterConfig {
                listen,
                advertise,
                join: args.join,
                files: ClusterFiles {
                    // clap's requires_all guarantees these with --advertise
                    cert: args.cluster_cert.expect("--cluster-cert"),
                    key: args.cluster_key.expect("--cluster-key"),
                    ca: args.cluster_ca.expect("--cluster-ca"),
                    secret: args.cluster_secret_file.expect("--cluster-secret-file"),
                },
            })
        }
    };
    let config = Config {
        bind_addr: args.bind,
        db_path: args.database,
        static_dir: args.static_dir,
        cluster,
    };

    // Signals are handled from here on, startup included
    let mut stop = mudd::shutdown_on_signals();
    let server = tokio::select! {
        server = Server::new(config) => server?,
        _ = stop.wait_for(|stop| *stop) => {
            tracing::info!("stopped during startup");
            return Ok(());
        }
    };
    server.run(stop).await?;

    Ok(())
}
