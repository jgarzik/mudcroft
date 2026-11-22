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
//! SQLite's write lock and the process-wide write gate: runs that write take
//! turns (the primary is the single writer anyway), so a failed run can
//! rebuild the caches from the tables without racing another run's pending
//! change. Caches are updated as writes happen (so the run sees its own
//! changes); a run that fails after writing has its `on_abort` rebuild them.
//!
//! Something slow outside the database (a Venice call) must not happen while
//! the lock is held: [`Run::checkpoint`] commits the run's writes so far and
//! releases it; the run then continues as a new change.
//!
//! Settling a run (commit or undo) happens in its own task, so a caller that
//! is cancelled part way (a client disconnecting) cannot leave the local
//! transaction open or the caches out of step.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::{Sqlite, SqliteConnection, SqlitePool};
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

/// A SQL statement with its bound parameters
pub type Statement = (String, Vec<serde_json::Value>);

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type AfterCommit = Box<dyn FnOnce() -> BoxFuture + Send>;

tokio::task_local! {
    static RUN: Arc<Run>;
}

/// Held by the run that is writing (see the module docs)
static WRITE_GATE: Mutex<()> = Mutex::const_new(());

/// How a run's writes are committed
pub trait Committer: Send + Sync {
    fn commit<'a>(
        &'a self,
        statements: Vec<Statement>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// One logical change in progress
pub struct Run {
    conn: Arc<Mutex<PoolConnection<Sqlite>>>,
    committer: Arc<dyn Committer>,
    gate: Mutex<Option<MutexGuard<'static, ()>>>,
    /// A local transaction is open (since the last checkpoint)
    began: AtomicBool,
    /// Caches may hold changes not in the tables (written since the last
    /// successful checkpoint): undoing the run must rebuild them
    dirty: AtomicBool,
    /// A checkpoint failed to save: writes made before it are lost, so the
    /// run must fail as a whole (later writes are refused)
    poisoned: AtomicBool,
    /// Bumped by each checkpoint (which ends the savepoints of nested runs)
    generation: AtomicU64,
    /// Nesting depth (names nested runs' savepoints)
    depth: AtomicU64,
    /// Leads the first batch committed (e.g. a command id marker)
    prefix: std::sync::Mutex<Vec<Statement>>,
    statements: std::sync::Mutex<Vec<Statement>>,
    /// Statements that stand even if the run fails: committed on their own
    /// then
    kept: std::sync::Mutex<Vec<Statement>>,
    after: std::sync::Mutex<Vec<AfterCommit>>,
}

fn take<T>(m: &std::sync::Mutex<Vec<T>>) -> Vec<T> {
    std::mem::take(&mut *m.lock().unwrap_or_else(|e| e.into_inner()))
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
        if self.poisoned.load(Ordering::SeqCst) {
            anyhow::bail!("an earlier save in this run failed; nothing more is saved");
        }
        {
            let mut gate = self.gate.lock().await;
            if gate.is_none() {
                *gate = Some(WRITE_GATE.lock().await);
            }
        }
        let mut conn = self.conn.lock().await;
        if !self.began.load(Ordering::SeqCst) {
            sqlx::query("BEGIN IMMEDIATE").execute(&mut **conn).await?;
            self.began.store(true, Ordering::SeqCst);
        }
        self.dirty.store(true, Ordering::SeqCst);
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

    /// End the local transaction (its writes come back through Raft)
    async fn end_local(&self) -> Result<()> {
        if self.began.swap(false, Ordering::SeqCst) {
            let mut conn = self.conn.lock().await;
            sqlx::query("ROLLBACK").execute(&mut **conn).await?;
        }
        Ok(())
    }

    /// The batch to commit: the prefix (first time only) and the captured
    /// statements, or nothing if there were none
    fn batch(&self) -> Vec<Statement> {
        let statements = take(&self.statements);
        if statements.is_empty() {
            return statements;
        }
        let mut batch = take(&self.prefix);
        batch.extend(statements);
        batch
    }

    /// Commit the run's writes so far as one change and release the write
    /// lock and gate; the run continues as a new change. For before
    /// something slow outside the database.
    pub async fn checkpoint(&self) -> Result<()> {
        if self.poisoned.load(Ordering::SeqCst) {
            anyhow::bail!("an earlier save in this run failed");
        }
        if let Err(e) = self.end_local().await {
            self.poisoned.store(true, Ordering::SeqCst);
            return Err(e);
        }
        let batch = self.batch();
        let kept = take(&self.kept);
        if !batch.is_empty() {
            if let Err(e) = self.committer.commit(batch).await {
                // The run fails as a whole (settle undoes it: `dirty` stays
                // set, so the caches are rebuilt); kept writes still stand
                self.poisoned.store(true, Ordering::SeqCst);
                take(&self.after);
                *self.kept.lock().unwrap_or_else(|e| e.into_inner()) = kept;
                return Err(e);
            }
        }
        self.dirty.store(false, Ordering::SeqCst);
        for f in take(&self.after) {
            f().await;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.gate.lock().await.take();
        Ok(())
    }

    /// Run `f` nested in this run: if it fails, only its own writes are
    /// undone (its kept ones stand) and `on_abort` rebuilds the caches,
    /// reading through this run (so the outer run's pending changes stay)
    async fn nested<T, E, Fut, A, AFut>(
        self: Arc<Self>,
        f: Fut,
        on_abort: A,
    ) -> Result<std::result::Result<T, E>>
    where
        Fut: Future<Output = std::result::Result<T, E>>,
        A: FnOnce() -> AFut,
        AFut: Future<Output = ()>,
    {
        let generation = self.generation.load(Ordering::SeqCst);
        let depth = self.depth.fetch_add(1, Ordering::SeqCst) + 1;
        let savepoint = format!("run_nested_{}", depth);
        let had_txn = self.began.load(Ordering::SeqCst);
        if had_txn {
            let mut conn = self.conn.lock().await;
            sqlx::query(sqlx::AssertSqlSafe(format!("SAVEPOINT {}", savepoint)))
                .execute(&mut **conn)
                .await?;
        }
        let marks = (
            self.statements
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            self.kept.lock().unwrap_or_else(|e| e.into_inner()).len(),
            self.after.lock().unwrap_or_else(|e| e.into_inner()).len(),
        );
        let dirty_before = self.dirty.swap(false, Ordering::SeqCst);

        let result = f.await;
        self.depth.fetch_sub(1, Ordering::SeqCst);
        let wrote = self.dirty.load(Ordering::SeqCst);
        // A checkpoint inside committed (and ended) everything before it
        let same_txn = self.generation.load(Ordering::SeqCst) == generation;
        let marks = if same_txn { marks } else { (0, 0, 0) };

        if result.is_ok() {
            if had_txn && same_txn {
                let mut conn = self.conn.lock().await;
                sqlx::query(sqlx::AssertSqlSafe(format!("RELEASE {}", savepoint)))
                    .execute(&mut **conn)
                    .await?;
            }
            self.dirty.store(dirty_before || wrote, Ordering::SeqCst);
            return Ok(result);
        }

        // Undo this part: locally, and in what will be committed
        if had_txn && same_txn {
            let mut conn = self.conn.lock().await;
            sqlx::query(sqlx::AssertSqlSafe(format!("ROLLBACK TO {}", savepoint)))
                .execute(&mut **conn)
                .await?;
            sqlx::query(sqlx::AssertSqlSafe(format!("RELEASE {}", savepoint)))
                .execute(&mut **conn)
                .await?;
        } else if self.began.swap(false, Ordering::SeqCst) {
            // Everything in the open transaction is this part's
            let mut conn = self.conn.lock().await;
            sqlx::query("ROLLBACK").execute(&mut **conn).await?;
        }
        self.statements
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .truncate(marks.0);
        let kept: Vec<Statement> = self
            .kept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .split_off(marks.1);
        self.after
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .truncate(marks.2);
        // Kept writes stand: redo them in the run
        if !kept.is_empty() {
            self.execute_kept(&kept).await?;
        }
        if wrote {
            on_abort().await;
        }
        self.dirty
            .store(dirty_before || !kept.is_empty(), Ordering::SeqCst);
        Ok(result)
    }

    /// Commit (or, `failed`, undo) the run, then release the gate. Returns
    /// a commit error (nothing but kept statements was saved then).
    async fn settle<A, AFut>(self: Arc<Self>, failed: bool, on_abort: A) -> Result<()>
    where
        A: FnOnce() -> AFut,
        AFut: Future<Output = ()>,
    {
        let ended = self.end_local().await;
        let batch = self.batch();
        let kept = take(&self.kept);
        let after = take(&self.after);
        let undo = |kept: Vec<Statement>| async {
            if !kept.is_empty() {
                if let Err(e) = self.committer.commit(kept).await {
                    tracing::error!("committing a failed run's kept writes: {}", e);
                }
            }
            if self.dirty.load(Ordering::SeqCst) {
                on_abort().await;
            }
        };
        let outcome = if let Err(e) = ended {
            undo(kept).await;
            Err(e)
        } else if self.poisoned.load(Ordering::SeqCst) {
            undo(kept).await;
            Err(anyhow::anyhow!("a save in this run failed"))
        } else if failed {
            undo(kept).await;
            Ok(())
        } else if batch.is_empty() {
            Ok(())
        } else if let Err(e) = self.committer.commit(batch).await {
            undo(kept).await;
            Err(e)
        } else {
            for f in after {
                f().await;
            }
            Ok(())
        };
        self.gate.lock().await.take();
        outcome
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

/// Settles a run whose caller was cancelled before it finished: undone, in
/// a task of its own
struct Unsettled<A>
where
    A: FnOnce() -> BoxFuture + Send + 'static,
{
    run: Option<Arc<Run>>,
    on_abort: Option<A>,
}

impl<A> Drop for Unsettled<A>
where
    A: FnOnce() -> BoxFuture + Send + 'static,
{
    fn drop(&mut self) {
        if let (Some(run), Some(on_abort)) = (self.run.take(), self.on_abort.take()) {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(e) = run.settle(true, on_abort).await {
                        tracing::error!("undoing a cancelled run: {}", e);
                    }
                });
            }
        }
    }
}

/// Run `f` as one change: its writes are proposed as one batch when it
/// succeeds, discarded when it fails. Inside a run already, `f` is a nested
/// part of it: if `f` fails, only its own writes are undone. `prefix` statements (e.g. a command's id marker) lead the batch.
/// When writes are discarded (`f` failed, or committing did), `on_abort`
/// runs before the write gate is released: it rebuilds the caches.
///
/// Returns `f`'s result, or an error if committing failed (then nothing but
/// kept statements was saved).
pub async fn scope<T, E, Fut, A, AFut>(
    pool: &SqlitePool,
    committer: Arc<dyn Committer>,
    prefix: Vec<Statement>,
    f: Fut,
    on_abort: A,
) -> Result<std::result::Result<T, E>>
where
    Fut: Future<Output = std::result::Result<T, E>>,
    A: FnOnce() -> AFut + Send + 'static,
    AFut: Future<Output = ()> + Send + 'static,
{
    if let Some(run) = current() {
        return run.nested(f, on_abort).await;
    }
    let run = Arc::new(Run {
        conn: Arc::new(Mutex::new(pool.acquire().await?)),
        committer,
        gate: Mutex::new(None),
        began: AtomicBool::new(false),
        dirty: AtomicBool::new(false),
        poisoned: AtomicBool::new(false),
        generation: AtomicU64::new(0),
        depth: AtomicU64::new(0),
        prefix: std::sync::Mutex::new(prefix),
        statements: std::sync::Mutex::new(Vec::new()),
        kept: std::sync::Mutex::new(Vec::new()),
        after: std::sync::Mutex::new(Vec::new()),
    });
    let boxed = move || -> BoxFuture { Box::pin(on_abort()) };
    let mut guard = Unsettled {
        run: Some(run.clone()),
        on_abort: Some(boxed),
    };
    let result = RUN.scope(run.clone(), f).await;
    // Settled in a task of its own: finishes even if this caller is
    // cancelled while waiting
    let on_abort = guard.on_abort.take().expect("set above");
    guard.run = None;
    let failed = result.is_err();
    match tokio::spawn(run.settle(failed, on_abort)).await {
        Ok(Ok(())) => Ok(result),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(anyhow::anyhow!("settling a run: {}", e)),
    }
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
        let direct: Arc<dyn Committer> = Arc::new(Direct(pool.clone()));
        let aborted = Arc::new(AtomicBool::new(false));
        let flag = aborted.clone();
        let on_abort = move || async move { flag.store(true, Ordering::SeqCst) };

        // Fails: only the kept statement stands, and the caches are rebuilt
        let result: std::result::Result<(), &str> = scope(
            &pool,
            direct.clone(),
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
        assert!(aborted.load(Ordering::SeqCst));

        // Succeeds: everything stands, and the run read its own write
        let seen: std::result::Result<i64, &str> = scope(
            &pool,
            direct.clone(),
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

    async fn setup() -> (SqlitePool, Arc<dyn Committer>) {
        let pool = crate::db::test_utils::test_pool().await;
        sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        let direct: Arc<dyn Committer> = Arc::new(Direct(pool.clone()));
        (pool, direct)
    }

    /// A checkpoint saves the writes so far and releases the write lock
    /// (others can write); a later failure undoes only what came after
    #[tokio::test(flavor = "multi_thread")]
    async fn test_checkpoint() {
        let (pool, direct) = setup().await;
        let result: std::result::Result<(), &str> = scope(
            &pool,
            direct.clone(),
            Vec::new(),
            async {
                let run = current().unwrap();
                run.execute(&[insert(1)]).await.unwrap();
                run.checkpoint().await.unwrap();
                // Saved, and the lock is free: a write outside the run works
                direct.commit(vec![insert(10)]).await.unwrap();
                run.execute(&[insert(2)]).await.unwrap();
                Err("failed")
            },
            || async {},
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(rows(&pool).await, vec![1, 10]);
    }

    /// A run whose caller is cancelled part way is undone: nothing saved,
    /// the caches rebuilt, the write lock released
    #[tokio::test(flavor = "multi_thread")]
    async fn test_cancelled_run_is_undone() {
        let (pool, direct) = setup().await;
        let aborted = Arc::new(AtomicBool::new(false));
        let flag = aborted.clone();
        let run = scope(
            &pool,
            direct.clone(),
            Vec::new(),
            async {
                current().unwrap().execute(&[insert(1)]).await.unwrap();
                std::future::pending::<()>().await;
                std::result::Result::<(), ()>::Ok(())
            },
            move || async move { flag.store(true, Ordering::SeqCst) },
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), run)
                .await
                .is_err(),
            "cancelled"
        );
        // The undo runs in a task of its own; a later write waits for it
        direct.commit(vec![insert(2)]).await.unwrap();
        for _ in 0..50 {
            if aborted.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(aborted.load(Ordering::SeqCst), "caches rebuilt");
        assert_eq!(rows(&pool).await, vec![2]);
    }

    /// A nested run that fails undoes only its own writes (its kept ones
    /// stand) and rebuilds the caches; the outer run goes on and commits
    #[tokio::test(flavor = "multi_thread")]
    async fn test_nested_run_failure() {
        let (pool, direct) = setup().await;
        let aborted = Arc::new(AtomicBool::new(false));
        let flag = aborted.clone();
        let inner_pool = pool.clone();
        let inner_direct = direct.clone();
        let result: std::result::Result<(), ()> = scope(
            &pool,
            direct.clone(),
            Vec::new(),
            async move {
                let run = current().unwrap();
                run.execute(&[insert(1)]).await.unwrap();
                let inner: std::result::Result<(), &str> = scope(
                    &inner_pool,
                    inner_direct,
                    Vec::new(),
                    async {
                        let run = current().unwrap();
                        run.execute(&[insert(2)]).await.unwrap();
                        run.execute_kept(&[insert(5)]).await.unwrap();
                        Err("inner failed")
                    },
                    move || async move { flag.store(true, Ordering::SeqCst) },
                )
                .await
                .unwrap();
                assert!(inner.is_err());
                run.execute(&[insert(3)]).await.unwrap();
                Ok(())
            },
            || async {},
        )
        .await
        .unwrap();
        assert!(result.is_ok());
        assert!(
            aborted.load(Ordering::SeqCst),
            "the inner run rebuilt caches"
        );
        assert_eq!(rows(&pool).await, vec![1, 3, 5]);
    }

    /// Refuses every commit (a node that lost leadership)
    struct Refuse;

    impl Committer for Refuse {
        fn commit<'a>(
            &'a self,
            _statements: Vec<Statement>,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async { anyhow::bail!("not the primary") })
        }
    }

    /// A checkpoint that fails to save poisons the run: later writes fail,
    /// the run counts as failed even if its code carried on, and the caches
    /// are rebuilt
    #[tokio::test(flavor = "multi_thread")]
    async fn test_failed_checkpoint_poisons_run() {
        let (pool, _) = setup().await;
        let aborted = Arc::new(AtomicBool::new(false));
        let flag = aborted.clone();
        let result: Result<std::result::Result<(), ()>> = scope(
            &pool,
            Arc::new(Refuse),
            Vec::new(),
            async {
                let run = current().unwrap();
                run.execute(&[insert(1)]).await.unwrap();
                assert!(run.checkpoint().await.is_err());
                // Code that catches the error and carries on
                assert!(run.execute(&[insert(2)]).await.is_err(), "poisoned");
                Ok(())
            },
            move || async move { flag.store(true, Ordering::SeqCst) },
        )
        .await;
        assert!(
            result.is_err() || result.unwrap().is_err(),
            "the run failed"
        );
        assert!(aborted.load(Ordering::SeqCst), "caches rebuilt");
        assert!(rows(&pool).await.is_empty());
    }
}
