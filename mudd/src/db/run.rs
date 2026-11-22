//! Run transactions: one logical change is one atomic Raft batch.
//!
//! A command, a Lua run or a timer callback runs inside a [`Run`]. Its
//! writes (everything that goes through `RaftWriter`) are executed at once
//! on the run's own connection, inside a local SQLite transaction, so the
//! run's later reads see them; they are also captured. When the run
//! succeeds, the local transaction is rolled back and the captured
//! statements are proposed to Raft as one entry (applied like any other,
//! on every node); only then do cache updates registered with
//! [`after_commit`] take effect. When it fails, nothing is kept: no
//! statement, no cache change.
//!
//! From its first write until it has committed (or been undone), a run holds
//! the process-wide write gate: runs that write take turns (the primary is
//! the single writer anyway), so a failed run can rebuild the caches from
//! the tables without racing another run's pending change. Caches are
//! updated as writes happen (so the run sees its own changes); a run that
//! fails after writing has its `on_abort` rebuild them.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::{Sqlite, SqliteConnection, SqlitePool};
use tokio::sync::{Mutex, OwnedMutexGuard};

/// A SQL statement with its bound parameters
pub type Statement = (String, Vec<serde_json::Value>);

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type AfterCommit = Box<dyn FnOnce() -> BoxFuture + Send>;

tokio::task_local! {
    static RUN: Arc<Run>;
}

/// Held by the run that is writing (see the module docs)
static WRITE_GATE: Mutex<()> = Mutex::const_new(());

/// One logical change in progress
pub struct Run {
    conn: Arc<Mutex<PoolConnection<Sqlite>>>,
    gate: Mutex<Option<tokio::sync::MutexGuard<'static, ()>>>,
    began: std::sync::atomic::AtomicBool,
    statements: std::sync::Mutex<Vec<Statement>>,
    /// Statements that stand even if the run fails (e.g. paying for an
    /// external call already made): committed on their own then
    kept: std::sync::Mutex<Vec<Statement>>,
    after: std::sync::Mutex<Vec<AfterCommit>>,
}

/// The run this task is in, if any
pub fn current() -> Option<Arc<Run>> {
    RUN.try_with(|run| run.clone()).ok()
}

impl Run {
    /// Execute statements on the run's connection (visible to its later
    /// reads) and capture them. All or none: a failing statement undoes the
    /// others in this call. Returns the rows affected.
    pub async fn execute(&self, statements: &[Statement]) -> Result<u64> {
        {
            let mut gate = self.gate.lock().await;
            if gate.is_none() {
                *gate = Some(WRITE_GATE.lock().await);
            }
        }
        let mut conn = self.conn.lock().await;
        if !self.began.swap(true, std::sync::atomic::Ordering::SeqCst) {
            sqlx::query("BEGIN IMMEDIATE").execute(&mut **conn).await?;
        }
        sqlx::query("SAVEPOINT run_call")
            .execute(&mut **conn)
            .await?;
        let mut rows = 0;
        for (sql, params) in statements {
            // Statements are built from literals in this crate, values bound
            let query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
            match crate::raft::storage::bind_params(query, params)
                .execute(&mut **conn)
                .await
            {
                Ok(done) => rows += done.rows_affected(),
                Err(e) => {
                    sqlx::query("ROLLBACK TO run_call")
                        .execute(&mut **conn)
                        .await?;
                    sqlx::query("RELEASE run_call").execute(&mut **conn).await?;
                    return Err(e.into());
                }
            }
        }
        sqlx::query("RELEASE run_call").execute(&mut **conn).await?;
        self.statements
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(statements.iter().cloned());
        Ok(rows)
    }

    /// Like [`Run::execute`], but the statements stand even if the run
    /// fails (then they are committed on their own, after the rest is
    /// discarded)
    pub async fn execute_kept(&self, statements: &[Statement]) -> Result<u64> {
        let rows = self.execute(statements).await?;
        self.kept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(statements.iter().cloned());
        Ok(rows)
    }
}

/// A connection for reading: the run's (so it sees the run's writes), or
/// one from the pool outside a run
pub enum Reader {
    Run(OwnedMutexGuard<PoolConnection<Sqlite>>),
    Pool(PoolConnection<Sqlite>),
}

impl std::ops::Deref for Reader {
    type Target = SqliteConnection;
    fn deref(&self) -> &SqliteConnection {
        match self {
            Reader::Run(guard) => guard,
            Reader::Pool(conn) => conn,
        }
    }
}

impl std::ops::DerefMut for Reader {
    fn deref_mut(&mut self) -> &mut SqliteConnection {
        match self {
            Reader::Run(guard) => &mut *guard,
            Reader::Pool(conn) => &mut *conn,
        }
    }
}

/// A connection to read with (see [`Reader`])
pub async fn reader(pool: &SqlitePool) -> Result<Reader> {
    match current() {
        Some(run) => Ok(Reader::Run(run.conn.clone().lock_owned().await)),
        None => Ok(Reader::Pool(pool.acquire().await?)),
    }
}

/// Do `f` once the current run commits (at once outside a run; never if the
/// run fails): for cache updates that must match what the tables hold
pub async fn after_commit<F, Fut>(f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    match current() {
        Some(run) => run
            .after
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(move || Box::pin(f()))),
        None => f().await,
    }
}

/// How a run's writes are committed
pub trait Committer: Send + Sync {
    fn commit<'a>(
        &'a self,
        statements: Vec<Statement>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// Run `f` as one change: its writes are proposed as one batch when it
/// succeeds, discarded when it fails. Inside a run already, `f` simply joins
/// it. `prefix` statements (e.g. a command's id marker) lead the batch.
/// When writes are discarded (`f` failed, or committing did), `on_abort`
/// runs before the write gate is released: it rebuilds the caches.
///
/// Returns `f`'s result, or an error if committing failed (then nothing was
/// kept either).
pub async fn scope<T, E, Fut, A, AFut>(
    pool: &SqlitePool,
    committer: &dyn Committer,
    prefix: Vec<Statement>,
    f: Fut,
    on_abort: A,
) -> Result<std::result::Result<T, E>>
where
    Fut: Future<Output = std::result::Result<T, E>>,
    A: FnOnce() -> AFut,
    AFut: Future<Output = ()>,
{
    if current().is_some() {
        return Ok(f.await);
    }
    let run = Arc::new(Run {
        conn: Arc::new(Mutex::new(pool.acquire().await?)),
        gate: Mutex::new(None),
        began: std::sync::atomic::AtomicBool::new(false),
        statements: std::sync::Mutex::new(Vec::new()),
        kept: std::sync::Mutex::new(Vec::new()),
        after: std::sync::Mutex::new(Vec::new()),
    });
    let result = RUN.scope(run.clone(), f).await;

    // End the local transaction either way: committed writes come back
    // through Raft
    if run.began.load(std::sync::atomic::Ordering::SeqCst) {
        let mut conn = run.conn.lock().await;
        sqlx::query("ROLLBACK").execute(&mut **conn).await?;
    }
    let statements = std::mem::take(&mut *run.statements.lock().unwrap_or_else(|e| e.into_inner()));
    let after = std::mem::take(&mut *run.after.lock().unwrap_or_else(|e| e.into_inner()));
    let kept = std::mem::take(&mut *run.kept.lock().unwrap_or_else(|e| e.into_inner()));
    // Held until the outcome is settled (released when dropped)
    let _gate = run.gate.lock().await.take();
    let wrote = run.began.load(std::sync::atomic::Ordering::SeqCst);
    drop(run);
    let commit_kept = |kept: Vec<Statement>| async move {
        if !kept.is_empty() {
            if let Err(e) = committer.commit(kept).await {
                tracing::error!("committing a failed run's kept writes: {}", e);
            }
        }
    };
    if result.is_err() {
        commit_kept(kept).await;
        if wrote {
            on_abort().await;
        }
        return Ok(result);
    }
    if !statements.is_empty() {
        let mut batch = prefix;
        batch.extend(statements);
        if let Err(e) = committer.commit(batch).await {
            commit_kept(kept).await;
            on_abort().await;
            return Err(e);
        }
    }
    for f in after {
        f().await;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies batches straight to the pool, in one transaction
    struct Direct(SqlitePool);

    impl Committer for Direct {
        fn commit<'a>(
            &'a self,
            statements: Vec<Statement>,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async move {
                let mut tx = self.0.begin().await?;
                for (sql, params) in &statements {
                    let query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                    crate::raft::storage::bind_params(query, params)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
                Ok(())
            })
        }
    }

    fn insert(x: i64) -> Statement {
        ("INSERT INTO t (x) VALUES (?)".to_string(), vec![x.into()])
    }

    async fn rows(pool: &SqlitePool) -> Vec<i64> {
        sqlx::query_scalar("SELECT x FROM t ORDER BY x")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_run_is_one_change() {
        let pool = crate::db::test_utils::test_pool().await;
        sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        let direct = Direct(pool.clone());
        let aborted = std::sync::atomic::AtomicBool::new(false);
        let on_abort = || async { aborted.store(true, std::sync::atomic::Ordering::SeqCst) };

        // Fails: only the kept statement stands, and the caches are rebuilt
        let result: std::result::Result<(), &str> = scope(
            &pool,
            &direct,
            Vec::new(),
            async {
                let run = current().unwrap();
                run.execute(&[insert(1)]).await.unwrap();
                run.execute_kept(&[insert(2)]).await.unwrap();
                Err("failed")
            },
            on_abort,
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(rows(&pool).await, vec![2]);
        assert!(aborted.load(std::sync::atomic::Ordering::SeqCst));

        // Succeeds: everything stands, and the run read its own write
        let seen: std::result::Result<i64, &str> = scope(
            &pool,
            &direct,
            Vec::new(),
            async {
                let run = current().unwrap();
                run.execute(&[insert(3)]).await.unwrap();
                // A failing statement undoes only its own call
                assert!(run
                    .execute(&[insert(4), ("BOGUS".to_string(), vec![])])
                    .await
                    .is_err());
                let mut conn = reader(&pool).await.unwrap();
                let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap();
                Ok(n)
            },
            || async {},
        )
        .await
        .unwrap();
        assert_eq!(
            seen,
            Ok(2),
            "kept row plus its own write, not the undone one"
        );
        assert_eq!(rows(&pool).await, vec![2, 3]);
    }
}
