//! TestCluster: several real mudd processes forming one Raft cluster
//!
//! Certificates are made with the `openssl` CLI, as an operator would (one
//! CA; one node certificate with subjectAltName DNS:mudd-cluster, shared by
//! the nodes), plus a shared secret file. Node 1 starts from a normal
//! `mudd_init`; every other node from `mudd_init --empty`, joining node 1.

use std::fs::File;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use reqwest::Client;
use tempfile::TempDir;

use super::server::{die_with_parent, find_binary_path, log_tail, spawn_mudd, ChildGuard};

/// One cluster member
pub struct ClusterNode {
    pub http: SocketAddr,
    pub cluster_addr: String,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    args: Vec<String>,
    child: ChildGuard,
}

impl ClusterNode {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.http)
    }
}

/// Cluster files made with openssl
pub struct ClusterCerts {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
    pub secret: PathBuf,
}

/// A cluster of mudd processes in one temp dir
pub struct TestCluster {
    pub dir: TempDir,
    pub certs: ClusterCerts,
    pub nodes: Vec<ClusterNode>,
    pub client: Client,
    mudd: PathBuf,
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

fn openssl(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "openssl {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// A CA and a node certificate for DNS:mudd-cluster, and a secret
pub fn make_certs(dir: &Path, secret: &str) -> Result<ClusterCerts> {
    let ec = [
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
    ];
    let mut ca = vec!["req", "-x509"];
    ca.extend_from_slice(&ec);
    ca.extend_from_slice(&[
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
        "-days",
        "2",
        "-subj",
        "/CN=mudd-test-ca",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ]);
    openssl(dir, &ca)?;
    let mut csr = vec!["req"];
    csr.extend_from_slice(&ec);
    csr.extend_from_slice(&[
        "-keyout",
        "node.key",
        "-out",
        "node.csr",
        "-subj",
        "/CN=mudd-cluster",
    ]);
    openssl(dir, &csr)?;
    std::fs::write(
        dir.join("node.ext"),
        "subjectAltName=DNS:mudd-cluster\nbasicConstraints=CA:FALSE\n\
         keyUsage=digitalSignature\nextendedKeyUsage=serverAuth,clientAuth\n",
    )?;
    openssl(
        dir,
        &[
            "x509",
            "-req",
            "-in",
            "node.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "node.pem",
            "-days",
            "2",
            "-extfile",
            "node.ext",
        ],
    )?;
    std::fs::write(dir.join("secret"), format!("{}\n", secret))?;
    Ok(ClusterCerts {
        cert: dir.join("node.pem"),
        key: dir.join("node.key"),
        ca: dir.join("ca.pem"),
        secret: dir.join("secret"),
    })
}

impl TestCluster {
    /// Start `n` nodes: node 1, then each other node joining it
    pub async fn start(n: usize) -> Result<Self> {
        let dir = TempDir::new()?;
        let certs = make_certs(dir.path(), "cluster-test-secret-0123456789")?;
        let mut cluster = Self {
            client: Client::builder().timeout(Duration::from_secs(5)).build()?,
            mudd: find_binary_path("mudd")?,
            certs,
            dir,
            nodes: Vec::new(),
        };
        cluster.add_first().await?;
        for _ in 1..n {
            cluster.add_joiner(None).await?;
        }
        Ok(cluster)
    }

    fn cluster_args(&self, cluster_port: u16, secret: Option<&Path>) -> Vec<String> {
        let addr = format!("127.0.0.1:{}", cluster_port);
        vec![
            "--advertise".into(),
            addr.clone(),
            "--cluster-listen".into(),
            addr,
            "--cluster-cert".into(),
            self.certs.cert.display().to_string(),
            "--cluster-key".into(),
            self.certs.key.display().to_string(),
            "--cluster-ca".into(),
            self.certs.ca.display().to_string(),
            "--cluster-secret-file".into(),
            secret.unwrap_or(&self.certs.secret).display().to_string(),
        ]
    }

    fn run_init(&self, db: &Path, empty: bool) -> Result<()> {
        let init = find_binary_path("mudd_init")?;
        let log = self.dir.path().join(format!(
            "{}.init.log",
            db.file_stem().unwrap().to_string_lossy()
        ));
        let out = File::create(&log)?;
        let mut cmd = Command::new(&init);
        die_with_parent(&mut cmd)
            .arg("--database")
            .arg(db)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out.try_clone()?))
            .stderr(Stdio::from(out));
        if empty {
            cmd.arg("--empty");
        } else {
            cmd.arg("--lib-dir")
                .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib"))
                .env("MUDD_ADMIN_USERNAME", "cluster_admin")
                .env("MUDD_ADMIN_PASSWORD", "testpass123");
        }
        let status = ChildGuard::new(cmd.spawn()?).wait()?;
        if !status.success() {
            anyhow::bail!("mudd_init failed:\n{}", log_tail(&log, 30));
        }
        Ok(())
    }

    async fn launch(&mut self, name: &str, args: Vec<String>, cluster_addr: String) -> Result<()> {
        let db_path = self.dir.path().join(format!("{}.db", name));
        let log_path = self.dir.path().join(format!("{}.log", name));
        let http: SocketAddr = format!("127.0.0.1:{}", free_port()?).parse()?;
        let child = spawn_mudd(
            &self.client,
            &self.mudd,
            http,
            &db_path,
            &args,
            &log_path,
            Some(self.dir.path()),
        )
        .await?;
        self.nodes.push(ClusterNode {
            http,
            cluster_addr,
            db_path,
            log_path,
            args,
            child,
        });
        Ok(())
    }

    async fn add_first(&mut self) -> Result<()> {
        self.run_init(&self.dir.path().join("node1.db"), false)?;
        let port = free_port()?;
        let args = self.cluster_args(port, None);
        self.launch("node1", args, format!("127.0.0.1:{}", port))
            .await
    }

    /// Start a new empty node joining node 1 (with another secret file, if
    /// given); its index in `nodes`
    pub async fn add_joiner(&mut self, secret: Option<&Path>) -> Result<usize> {
        let name = format!("node{}", self.nodes.len() + 1);
        self.run_init(&self.dir.path().join(format!("{}.db", name)), true)?;
        let port = free_port()?;
        let mut args = self.cluster_args(port, secret);
        args.push("--join".into());
        args.push(self.nodes[0].cluster_addr.clone());
        self.launch(&name, args, format!("127.0.0.1:{}", port))
            .await?;
        Ok(self.nodes.len() - 1)
    }

    /// Run `mudd_admin cluster --node <node i> ... <args>`: (success, stdout
    /// and stderr)
    pub fn admin(&self, i: usize, args: &[&str]) -> Result<(bool, String)> {
        let out = Command::new(find_binary_path("mudd_admin")?)
            .arg("cluster")
            .arg("--node")
            .arg(&self.nodes[i].cluster_addr)
            .arg("--cluster-ca")
            .arg(&self.certs.ca)
            .arg("--cluster-secret-file")
            .arg(&self.certs.secret)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        Ok((
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        ))
    }

    /// Put a Lua script where run_script finds it (`scripts/<name>` under
    /// the nodes' working directory)
    pub fn add_script(&self, name: &str, source: &str) -> Result<()> {
        let dir = self.dir.path().join("scripts");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(name), source)?;
        Ok(())
    }

    /// The nodes whose log says they became primary
    pub fn primaries(&self) -> Vec<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                std::fs::read_to_string(&n.log_path)
                    .map(|log| log.contains("is the primary"))
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The last lines of every node's log (for failure messages)
    pub fn logs(&self) -> String {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| format!("---- node {} ----\n{}", i + 1, log_tail(&n.log_path, 25)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Stop node `i` (its database stays)
    pub fn stop(&mut self, i: usize) {
        self.nodes[i].child.kill();
    }

    /// Start node `i` again with the same arguments and database
    pub async fn restart(&mut self, i: usize) -> Result<()> {
        let node = &self.nodes[i];
        let child = spawn_mudd(
            &self.client,
            &self.mudd,
            node.http,
            &node.db_path,
            &node.args,
            &node.log_path,
            Some(self.dir.path()),
        )
        .await?;
        self.nodes[i].child = child;
        Ok(())
    }

    /// A read connection to node `i`'s database
    pub async fn pool(&self, i: usize) -> Result<sqlx::SqlitePool> {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let options = SqliteConnectOptions::new()
            .filename(&self.nodes[i].db_path)
            .read_only(true);
        Ok(SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?)
    }

    /// Poll `query` (a single-value SELECT) on node `i` until it returns
    /// `want` or `limit` passes; the last value seen
    pub async fn wait_for(&self, i: usize, query: &str, want: i64, limit: Duration) -> i64 {
        let deadline = tokio::time::Instant::now() + limit;
        let mut last = -1;
        while tokio::time::Instant::now() < deadline {
            if let Ok(pool) = self.pool(i).await {
                if let Ok(v) = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query))
                    .fetch_one(&pool)
                    .await
                {
                    last = v;
                }
                pool.close().await;
                if last == want {
                    return last;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        last
    }
}
