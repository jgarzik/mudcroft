//! Lua sandbox - secure execution environment

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mlua::{Function, HookTriggers, Lua, Result as LuaResult, StdLib, Value, VmState};
use thiserror::Error;

use super::Metering;

/// Sandbox configuration
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Maximum instructions per execution (default: 1,000,000)
    pub max_instructions: u64,
    /// Maximum memory in bytes (default: 64MB)
    pub max_memory: usize,
    /// Execution timeout (default: 500ms)
    pub timeout: Duration,
    /// Maximum database queries per execution (default: 100)
    pub max_db_queries: u64,
    /// Maximum Venice API calls per execution (default: 5)
    pub max_venice_calls: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            max_instructions: 1_000_000,
            max_memory: 64 * 1024 * 1024, // 64MB
            timeout: Duration::from_millis(500),
            max_db_queries: 100,
            max_venice_calls: 5,
        }
    }
}

/// Per-execution budget for database queries and Venice calls.
///
/// Kept in the Lua state's app data so `game.*` functions can charge it
/// (see [`charge_db`] / [`charge_venice`]); reset at the start of every
/// execution.
#[derive(Debug, Default)]
pub(crate) struct CallBudget {
    db_queries: u64,
    venice_calls: u64,
    max_db_queries: u64,
    max_venice_calls: u64,
}

/// Count one database-backed `game.*` call against the execution's budget
pub(crate) fn charge_db(lua: &Lua) -> LuaResult<()> {
    if let Some(mut budget) = lua.app_data_mut::<CallBudget>() {
        budget.db_queries += 1;
        if budget.db_queries > budget.max_db_queries {
            return Err(mlua::Error::runtime(
                SandboxError::DbQueryLimitExceeded(budget.db_queries, budget.max_db_queries)
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// Count one Venice API call against the execution's budget
pub(crate) fn charge_venice(lua: &Lua) -> LuaResult<()> {
    if let Some(mut budget) = lua.app_data_mut::<CallBudget>() {
        budget.venice_calls += 1;
        if budget.venice_calls > budget.max_venice_calls {
            return Err(mlua::Error::runtime(
                SandboxError::VeniceCallLimitExceeded(budget.venice_calls, budget.max_venice_calls)
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// Errors that can occur during sandbox execution
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("instruction limit exceeded ({0} > {1})")]
    InstructionLimitExceeded(u64, u64),

    #[error("memory limit exceeded ({0} bytes > {1} bytes)")]
    MemoryLimitExceeded(usize, usize),

    #[error("execution timeout ({0:?})")]
    Timeout(Duration),

    #[error("database query limit exceeded ({0} > {1})")]
    DbQueryLimitExceeded(u64, u64),

    #[error("Venice API call limit exceeded ({0} > {1})")]
    VeniceCallLimitExceeded(u64, u64),

    #[error("Lua error: {0}")]
    LuaError(#[from] mlua::Error),

    /// A native function panicked (a server bug): the script fails, the
    /// task running it carries on. The message is for the server log only;
    /// scripts and players see "internal error".
    #[error("internal error")]
    Panic(String),
}

/// A sandboxed Lua execution environment
pub struct Sandbox {
    lua: Lua,
    config: SandboxConfig,
    metering: Metering,
    limits: Arc<Limits>,
}

/// Lua app data giving native functions the running execution's limits
struct SharedLimits(Arc<Limits>);

/// The per-execution budget, shared with the instruction hook and the
/// sandbox's pcall/xpcall
struct Limits {
    /// Fixed reference point for the deadline
    origin: Instant,
    /// Deadline, in nanoseconds after `origin`
    deadline_ns: AtomicU64,
    instructions: AtomicU64,
    max_instructions: u64,
    timeout: Duration,
    instructions_exceeded: AtomicBool,
    timed_out: AtomicBool,
}

impl Limits {
    fn begin(&self) {
        self.instructions.store(0, Ordering::Relaxed);
        self.instructions_exceeded.store(false, Ordering::Relaxed);
        self.timed_out.store(false, Ordering::Relaxed);
        let deadline = self.origin.elapsed() + self.timeout;
        self.deadline_ns
            .store(deadline.as_nanos() as u64, Ordering::Relaxed);
    }

    fn deadline(&self) -> Instant {
        self.origin + Duration::from_nanos(self.deadline_ns.load(Ordering::Relaxed))
    }

    /// Whether the execution must stop (either limit hit)
    fn stopped(&self) -> bool {
        self.instructions_exceeded.load(Ordering::Relaxed) || self.timed_out.load(Ordering::Relaxed)
    }
}

impl Sandbox {
    /// Create a new sandbox with the given configuration
    pub fn new(config: SandboxConfig) -> Result<Self, SandboxError> {
        // Create Lua with minimal standard libraries
        let lua = Lua::new_with(
            StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8,
            mlua::LuaOptions::default(),
        )?;

        // Set memory limit
        lua.set_memory_limit(config.max_memory)?;

        let limits = Arc::new(Limits {
            origin: Instant::now(),
            deadline_ns: AtomicU64::new(0),
            instructions: AtomicU64::new(0),
            max_instructions: config.max_instructions,
            timeout: config.timeout,
            instructions_exceeded: AtomicBool::new(false),
            timed_out: AtomicBool::new(false),
        });
        // One budget per execution, starting now (begin_execution restarts it)
        limits.begin();

        // Every 1000 instructions: count, and check the wall clock, so both
        // limits hold while code runs. Hitting either raises an error (a
        // yield does nothing on the non-yieldable main thread), and every
        // later check raises again, so a caught error cannot keep the
        // script running; the sandbox's pcall/xpcall also re-raise it.
        let hook_limits = limits.clone();
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(1000),
            move |_lua, _debug| {
                let l = &hook_limits;
                let current = l.instructions.fetch_add(1000, Ordering::Relaxed) + 1000;
                if current > l.max_instructions {
                    l.instructions_exceeded.store(true, Ordering::Relaxed);
                    return Err(mlua::Error::runtime("instruction limit exceeded"));
                }
                if l.origin.elapsed().as_nanos() as u64 > l.deadline_ns.load(Ordering::Relaxed) {
                    l.timed_out.store(true, Ordering::Relaxed);
                    return Err(mlua::Error::runtime("execution timeout"));
                }
                Ok(VmState::Continue)
            },
        )?;

        lua.set_app_data(CallBudget {
            max_db_queries: config.max_db_queries,
            max_venice_calls: config.max_venice_calls,
            ..Default::default()
        });

        // Nested sandboxes (use_object) find the live deadline here
        lua.set_app_data(SharedLimits(limits.clone()));

        // Remove dangerous globals
        Self::remove_dangerous_globals(&lua)?;

        // Add safe utility functions
        Self::add_safe_globals(&lua)?;
        Self::add_limit_aware_pcall(&lua, limits.clone())?;

        Ok(Self {
            lua,
            config,
            metering: Metering::new(),
            limits,
        })
    }

    /// When the current execution's time runs out
    pub fn deadline(&self) -> Instant {
        self.limits.deadline()
    }

    /// Never run past `at`: a nested sandbox (use_object) takes its caller's
    /// deadline, so nesting cannot extend a run's time
    pub fn cap_deadline(&mut self, at: Instant) {
        let cap = at.saturating_duration_since(self.limits.origin).as_nanos() as u64;
        self.limits.deadline_ns.fetch_min(cap, Ordering::Relaxed);
    }

    /// The deadline of the sandbox `lua` belongs to (for game.* functions
    /// that start a nested sandbox)
    pub fn deadline_of(lua: &Lua) -> Option<Instant> {
        lua.app_data_ref::<SharedLimits>().map(|l| l.0.deadline())
    }

    /// Start a fresh budget (instructions, deadline, call counts). A new
    /// sandbox starts with one; libraries, code and handler run in it share
    /// it. Call this only to reuse a sandbox for another execution.
    pub fn begin_execution(&mut self) {
        self.limits.begin();
        self.metering.reset();
        if let Some(mut budget) = self.lua.app_data_mut::<CallBudget>() {
            budget.db_queries = 0;
            budget.venice_calls = 0;
        }
    }

    /// Remove dangerous global functions/tables
    fn remove_dangerous_globals(lua: &Lua) -> LuaResult<()> {
        let globals = lua.globals();

        // Remove dangerous functions
        let dangerous = [
            "os",
            "io",
            "loadfile",
            "dofile",
            "load",
            "loadstring",
            "require",
            "package",
            "debug",
            "collectgarbage",
        ];

        for name in dangerous {
            globals.set(name, Value::Nil)?;
        }

        Ok(())
    }

    /// Add safe utility functions
    fn add_safe_globals(lua: &Lua) -> LuaResult<()> {
        let globals = lua.globals();

        // Add a safe print function that does nothing (or could log)
        let safe_print = lua.create_function(|_, args: mlua::MultiValue| {
            // In production, this could log to a debug buffer
            let _ = args;
            Ok(())
        })?;
        globals.set("print", safe_print)?;

        Ok(())
    }

    /// Replace pcall and xpcall with versions that catch ordinary errors but
    /// re-raise once the instruction limit is exceeded, so a script cannot
    /// catch the limit and keep running
    fn add_limit_aware_pcall(lua: &Lua, limits: Arc<Limits>) -> LuaResult<()> {
        use mlua::MultiValue;

        fn error_value(lua: &Lua, e: &mlua::Error) -> LuaResult<Value> {
            Ok(Value::String(lua.create_string(e.to_string())?))
        }

        let flag = limits.clone();
        let pcall = lua.create_function(move |lua, (f, args): (Function, MultiValue)| {
            match f.call::<MultiValue>(args) {
                Ok(mut values) => {
                    values.push_front(Value::Boolean(true));
                    Ok(values)
                }
                Err(e) if flag.stopped() => Err(e),
                Err(e) => Ok(MultiValue::from_vec(vec![
                    Value::Boolean(false),
                    error_value(lua, &e)?,
                ])),
            }
        })?;
        lua.globals().set("pcall", pcall)?;

        let flag = limits;
        let xpcall = lua.create_function(
            move |lua, (f, handler, args): (Function, Function, MultiValue)| match f
                .call::<MultiValue>(args)
            {
                Ok(mut values) => {
                    values.push_front(Value::Boolean(true));
                    Ok(values)
                }
                Err(e) if flag.stopped() => Err(e),
                Err(e) => {
                    let mut handled = handler.call::<MultiValue>(error_value(lua, &e)?)?;
                    handled.push_front(Value::Boolean(false));
                    Ok(handled)
                }
            },
        )?;
        lua.globals().set("xpcall", xpcall)?;
        Ok(())
    }

    /// The error a nested execution (game.use_object) hands its caller. A
    /// panic is not an ordinary error there: it resumes unwinding, so it
    /// passes through the caller's pcall and fails the whole top-level run
    /// (whose writes then roll back), where the outer entry point catches it.
    pub fn nested_error(e: SandboxError) -> SandboxError {
        match e {
            SandboxError::Panic(msg) => std::panic::resume_unwind(Box::new(msg)),
            e => e,
        }
    }

    /// Get metering data
    pub fn metering(&self) -> &Metering {
        &self.metering
    }

    /// Get the sandbox configuration
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Execute Lua code and return the result
    pub fn execute<R>(&mut self, code: &str) -> Result<R, SandboxError>
    where
        R: mlua::FromLuaMulti,
    {
        // `eval` also accepts bare expressions (e.g. `1 + 2`)
        self.run_metered(|lua| lua.load(code).eval())
    }

    /// Execute a Lua chunk, passing `args` as the chunk's varargs (`...`).
    ///
    /// Use this instead of formatting untrusted values into Lua source:
    /// the values arrive as Lua strings/numbers and can never change the
    /// code that runs.
    pub fn execute_with_args<A, R>(&mut self, code: &str, args: A) -> Result<R, SandboxError>
    where
        A: mlua::IntoLuaMulti,
        R: mlua::FromLuaMulti,
    {
        self.run_metered(|lua| lua.load(code).into_function()?.call(args))
    }

    /// Run `f` within the current budget (not reset per chunk)
    fn run_metered<R>(&mut self, f: impl FnOnce(&Lua) -> LuaResult<R>) -> Result<R, SandboxError> {
        let before = self.limits.instructions.load(Ordering::Relaxed);
        let lua = &self.lua;
        let result = catch_panic(|| f(lua));
        self.finish(before, result)
    }

    /// Account for a run and map a limit hit (or a caught panic) to its error
    fn finish<R>(
        &mut self,
        before: u64,
        result: Result<LuaResult<R>, String>,
    ) -> Result<R, SandboxError> {
        let instr = self.limits.instructions.load(Ordering::Relaxed);
        self.metering.add_instructions(instr.saturating_sub(before));
        self.metering.set_memory(self.lua.used_memory() as u64);

        if self.limits.instructions_exceeded.load(Ordering::Relaxed) {
            return Err(SandboxError::InstructionLimitExceeded(
                instr,
                self.config.max_instructions,
            ));
        }
        // Also past the deadline without the hook noticing: time spent in a
        // native call (a slow game.* function) with little Lua after it
        if self.limits.timed_out.load(Ordering::Relaxed) || Instant::now() > self.limits.deadline()
        {
            self.limits.timed_out.store(true, Ordering::Relaxed);
            return Err(SandboxError::Timeout(self.config.timeout));
        }
        match result {
            Ok(result) => result.map_err(SandboxError::from),
            Err(panic) => Err(SandboxError::Panic(panic)),
        }
    }

    /// Execute a Lua function with arguments, within the current budget
    pub fn call<A, R>(&mut self, func: Function, args: A) -> Result<R, SandboxError>
    where
        A: mlua::IntoLuaMulti,
        R: mlua::FromLuaMulti,
    {
        let before = self.limits.instructions.load(Ordering::Relaxed);
        let result = catch_panic(|| func.call(args));
        self.finish(before, result)
    }

    /// Check if a global exists (for testing that dangerous globals are removed)
    pub fn global_exists(&self, name: &str) -> bool {
        self.lua
            .globals()
            .get::<Value>(name)
            .map(|v| !matches!(v, Value::Nil))
            .unwrap_or(false)
    }

    /// Get the current instruction count
    pub fn instruction_count(&self) -> u64 {
        self.limits.instructions.load(Ordering::Relaxed)
    }

    /// Get current memory usage
    pub fn memory_used(&self) -> usize {
        self.lua.used_memory()
    }

    /// Access the underlying Lua state (for registering game functions)
    pub fn lua(&self) -> &Lua {
        &self.lua
    }
}

/// Run Lua, catching a panic from a native function (mlua resumes a
/// callback's panic once control is back in Rust). The panic becomes the
/// script's error, so one bad call never kills the task running scripts.
/// The sandbox is per execution and is dropped after the error.
fn catch_panic<R>(f: impl FnOnce() -> LuaResult<R>) -> Result<LuaResult<R>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        tracing::error!("lua: native function panicked: {}", msg);
        msg
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic inside a nested execution (game.use_object) fails the whole
    /// top-level run, so its writes roll back: the caller's pcall cannot
    /// catch it and carry on to commit a half-finished change
    #[test]
    fn test_nested_panic_not_swallowed_by_pcall() {
        let mut outer = Sandbox::new(SandboxConfig::default()).unwrap();
        let nested = outer
            .lua()
            .create_function(|_, ()| {
                let mut inner = Sandbox::new(SandboxConfig::default()).unwrap();
                let boom = inner
                    .lua()
                    .create_function(|_, ()| -> mlua::Result<()> { panic!("nested bug") })
                    .unwrap();
                inner.lua().globals().set("boom", boom).unwrap();
                inner
                    .execute::<()>("boom()")
                    .map_err(|e| mlua::Error::runtime(Sandbox::nested_error(e)))
            })
            .unwrap();
        outer.lua().globals().set("nested", nested).unwrap();
        match outer.execute::<mlua::Value>("pcall(nested) return 1") {
            Err(SandboxError::Panic(_)) => {}
            other => panic!("expected the run to fail with Panic, got {other:?}"),
        }
    }

    /// A panic in a native (game.*) function fails the script with an
    /// error; it never unwinds into the task running it (the timer loop,
    /// a scheduler, a session), and pcall cannot swallow it
    #[test]
    fn test_native_panic_is_an_error() {
        let mut sandbox = Sandbox::new(SandboxConfig::default()).unwrap();
        let boom = sandbox
            .lua()
            .create_function(|_, ()| -> mlua::Result<()> { panic!("native bug") })
            .unwrap();
        sandbox.lua().globals().set("boom", boom).unwrap();

        for code in ["boom()", "pcall(boom) return 1", "return 1 + (boom() or 0)"] {
            match sandbox.execute::<mlua::Value>(code) {
                Err(e @ SandboxError::Panic(_)) => {
                    // Details only in the server log
                    assert_eq!(e.to_string(), "internal error");
                    let SandboxError::Panic(msg) = e else {
                        unreachable!()
                    };
                    assert!(msg.contains("native bug"), "{msg}");
                }
                other => panic!("{code}: expected Panic, got {other:?}"),
            }
        }
        match sandbox.execute_with_args::<_, mlua::Value>("boom()", ()) {
            Err(SandboxError::Panic(_)) => {}
            other => panic!("execute_with_args: expected Panic, got {other:?}"),
        }
        let f: mlua::Function = sandbox.lua().globals().get("boom").unwrap();
        match sandbox.call::<_, ()>(f, ()) {
            Err(SandboxError::Panic(_)) => {}
            other => panic!("call: expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn test_sandbox_creation() {
        let sandbox = Sandbox::new(SandboxConfig::default()).unwrap();
        assert!(!sandbox.global_exists("os"));
        assert!(!sandbox.global_exists("io"));
    }

    #[test]
    fn test_dangerous_globals_removed() {
        let sandbox = Sandbox::new(SandboxConfig::default()).unwrap();

        // These should all be removed
        assert!(!sandbox.global_exists("os"));
        assert!(!sandbox.global_exists("io"));
        assert!(!sandbox.global_exists("loadfile"));
        assert!(!sandbox.global_exists("dofile"));
        assert!(!sandbox.global_exists("load"));
        assert!(!sandbox.global_exists("loadstring"));
        assert!(!sandbox.global_exists("require"));
        assert!(!sandbox.global_exists("package"));
        assert!(!sandbox.global_exists("debug"));

        // These should still exist
        assert!(sandbox.global_exists("string"));
        assert!(sandbox.global_exists("table"));
        assert!(sandbox.global_exists("math"));
        assert!(sandbox.global_exists("print")); // Safe version
    }

    #[test]
    fn test_simple_execution() {
        let mut sandbox = Sandbox::new(SandboxConfig::default()).unwrap();
        let result: i64 = sandbox.execute("return 1 + 2").unwrap();
        assert_eq!(result, 3);
    }

    #[test]
    fn test_instruction_limit() {
        let config = SandboxConfig {
            max_instructions: 100, // Very low limit
            ..Default::default()
        };
        let mut sandbox = Sandbox::new(config).unwrap();

        // This loop should exceed the instruction limit
        let result: Result<(), _> = sandbox.execute(
            r#"
            local sum = 0
            for i = 1, 1000000 do
                sum = sum + i
            end
            return sum
            "#,
        );

        assert!(matches!(
            result,
            Err(SandboxError::InstructionLimitExceeded(_, _))
        ));
    }

    /// Run `code` under a low instruction limit on its own thread; None if
    /// it is still running after the deadline (the limit failed to stop it)
    fn run_limited(code: &'static str) -> Option<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let config = SandboxConfig {
                max_instructions: 10_000,
                ..Default::default()
            };
            let mut sandbox = Sandbox::new(config).unwrap();
            let _ = tx.send(format!("{:?}", sandbox.execute::<()>(code)));
        });
        rx.recv_timeout(std::time::Duration::from_secs(10)).ok()
    }

    /// A script that never ends on its own is stopped by the limit
    #[test]
    fn test_instruction_limit_stops_infinite_loop() {
        let result = run_limited("while true do end").expect("the loop was not stopped");
        assert!(result.contains("InstructionLimitExceeded"), "{}", result);
    }

    /// pcall/xpcall cannot swallow the limit and keep running
    #[test]
    fn test_instruction_limit_not_caught_by_pcall() {
        for code in [
            "while true do pcall(function() while true do end end) end",
            "while true do xpcall(function() while true do end end, function(e) return e end) end",
        ] {
            let result = run_limited(code).expect("pcall kept the script alive");
            assert!(
                result.contains("InstructionLimitExceeded"),
                "{}: {}",
                code,
                result
            );
        }
    }

    /// Run `code` with a huge instruction limit and a short timeout on its
    /// own thread; None if it is still running after the deadline
    fn run_timed(code: &'static str) -> Option<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let config = SandboxConfig {
                max_instructions: u64::MAX / 2,
                timeout: Duration::from_millis(200),
                ..Default::default()
            };
            let mut sandbox = Sandbox::new(config).unwrap();
            let _ = tx.send(format!("{:?}", sandbox.execute::<()>(code)));
        });
        rx.recv_timeout(Duration::from_secs(10)).ok()
    }

    /// The wall-clock timeout stops code while it runs, even under pcall
    #[test]
    fn test_timeout_stops_running_code() {
        for code in [
            "while true do end",
            "while true do pcall(function() while true do end end) end",
        ] {
            let result = run_timed(code).expect("the timeout did not stop it");
            assert!(result.contains("Timeout"), "{}: {}", code, result);
        }
    }

    /// Time spent inside one native call (a slow game.* function) counts
    /// too, even if few Lua instructions run afterwards
    #[test]
    fn test_timeout_counts_time_in_native_calls() {
        let config = SandboxConfig {
            timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let mut sandbox = Sandbox::new(config).unwrap();
        let slow = sandbox
            .lua()
            .create_function(|_, ()| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .unwrap();
        sandbox.lua().globals().set("slow", slow).unwrap();
        let result = sandbox.execute::<i64>("slow() return 1");
        assert!(
            matches!(result, Err(SandboxError::Timeout(_))),
            "{:?}",
            result
        );
    }

    /// A nested sandbox (use_object) never gets more time than the run it
    /// is part of: it inherits the outer deadline
    #[test]
    fn test_nested_sandbox_inherits_deadline() {
        let outer = Sandbox::new(SandboxConfig {
            timeout: Duration::from_millis(100),
            ..Default::default()
        })
        .unwrap();
        let deadline = outer.deadline();
        let mut inner = Sandbox::new(SandboxConfig::default()).unwrap();
        inner.cap_deadline(deadline);
        std::thread::sleep(Duration::from_millis(150));
        let result = inner.execute::<i64>("local s = 0 for i = 1, 5000 do s = s + i end return s");
        assert!(
            matches!(result, Err(SandboxError::Timeout(_))),
            "{:?}",
            result
        );
    }

    /// One budget covers a whole execution: chunks run one after another in
    /// a sandbox (libraries, then code) share it rather than each getting
    /// their own
    #[test]
    fn test_budget_spans_the_execution() {
        let config = SandboxConfig {
            max_instructions: 30_000,
            ..Default::default()
        };
        let mut sandbox = Sandbox::new(config).unwrap();
        let chunk = "local s = 0 for i = 1, 4000 do s = s + i end";
        assert!(sandbox.execute::<()>(chunk).is_ok(), "first chunk fits");
        let mut failed = false;
        for _ in 0..10 {
            if sandbox.execute::<()>(chunk).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "repeated chunks must exhaust one shared budget");

        // begin_execution starts a fresh budget
        sandbox.begin_execution();
        assert!(sandbox.execute::<()>(chunk).is_ok());
    }

    /// pcall still catches ordinary errors
    #[test]
    fn test_pcall_catches_ordinary_errors() {
        let mut sandbox = Sandbox::new(SandboxConfig::default()).unwrap();
        let result: String = sandbox
            .execute(
                r#"local ok, err = pcall(function() error("boom") end)
                   local ok2, v = pcall(function(a, b) return a + b end, 2, 3)
                   return tostring(ok) .. ":" .. tostring(err):match("boom") .. ":" .. tostring(ok2) .. v"#,
            )
            .unwrap();
        assert_eq!(result, "false:boom:true5");
    }

    #[test]
    fn test_memory_limit() {
        let config = SandboxConfig {
            max_memory: 1024 * 1024, // 1MB limit
            ..Default::default()
        };
        let mut sandbox = Sandbox::new(config).unwrap();

        // Try to allocate a large table
        let result: Result<(), _> = sandbox.execute(
            r#"
            local t = {}
            for i = 1, 10000000 do
                t[i] = string.rep("x", 1000)
            end
            "#,
        );

        // Should fail with memory error
        assert!(result.is_err());
    }

    #[test]
    fn test_metering() {
        let mut sandbox = Sandbox::new(SandboxConfig::default()).unwrap();

        let _: () = sandbox
            .execute(
                r#"
            local sum = 0
            for i = 1, 1000 do
                sum = sum + i
            end
            "#,
            )
            .unwrap();

        // Should have recorded some instructions
        assert!(sandbox.metering().instructions() > 0);
    }

    #[test]
    fn test_string_operations() {
        let mut sandbox = Sandbox::new(SandboxConfig::default()).unwrap();
        let result: String = sandbox.execute(r#"return string.upper("hello")"#).unwrap();
        assert_eq!(result, "HELLO");
    }

    #[test]
    fn test_table_operations() {
        let mut sandbox = Sandbox::new(SandboxConfig::default()).unwrap();
        let result: i64 = sandbox
            .execute(
                r#"
            local t = {1, 2, 3, 4, 5}
            return #t
            "#,
            )
            .unwrap();
        assert_eq!(result, 5);
    }

    #[test]
    fn test_math_operations() {
        let mut sandbox = Sandbox::new(SandboxConfig::default()).unwrap();
        let result: f64 = sandbox.execute("return math.sqrt(16)").unwrap();
        assert!((result - 4.0).abs() < 0.001);
    }
}
