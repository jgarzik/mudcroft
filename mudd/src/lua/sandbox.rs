//! Lua sandbox - secure execution environment

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
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
}

/// A sandboxed Lua execution environment
pub struct Sandbox {
    lua: Lua,
    config: SandboxConfig,
    metering: Metering,
    instruction_count: Arc<AtomicU64>,
    exceeded: Arc<AtomicBool>,
    start_time: Option<Instant>,
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

        let instruction_count = Arc::new(AtomicU64::new(0));
        let exceeded = Arc::new(AtomicBool::new(false));

        // Set up instruction counting hook
        let count_clone = instruction_count.clone();
        let exceeded_clone = exceeded.clone();
        let max_instructions = config.max_instructions;

        lua.set_hook(
            HookTriggers::new().every_nth_instruction(1000),
            move |_lua, _debug| {
                let current = count_clone.fetch_add(1000, Ordering::Relaxed) + 1000;
                if current > max_instructions {
                    // Raise an error: yielding does nothing on the main
                    // thread, which is not yieldable. Every later check
                    // raises again, so even a caught error cannot keep the
                    // script running; the sandbox's pcall/xpcall re-raise it.
                    exceeded_clone.store(true, Ordering::Relaxed);
                    Err(mlua::Error::runtime("instruction limit exceeded"))
                } else {
                    Ok(VmState::Continue)
                }
            },
        )?;

        lua.set_app_data(CallBudget {
            max_db_queries: config.max_db_queries,
            max_venice_calls: config.max_venice_calls,
            ..Default::default()
        });

        // Remove dangerous globals
        Self::remove_dangerous_globals(&lua)?;

        // Add safe utility functions
        Self::add_safe_globals(&lua)?;
        Self::add_limit_aware_pcall(&lua, exceeded.clone())?;

        Ok(Self {
            lua,
            config,
            metering: Metering::new(),
            instruction_count,
            exceeded,
            start_time: None,
        })
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
    fn add_limit_aware_pcall(lua: &Lua, exceeded: Arc<AtomicBool>) -> LuaResult<()> {
        use mlua::MultiValue;

        fn error_value(lua: &Lua, e: &mlua::Error) -> LuaResult<Value> {
            Ok(Value::String(lua.create_string(e.to_string())?))
        }

        let flag = exceeded.clone();
        let pcall = lua.create_function(move |lua, (f, args): (Function, MultiValue)| {
            match f.call::<MultiValue>(args) {
                Ok(mut values) => {
                    values.push_front(Value::Boolean(true));
                    Ok(values)
                }
                Err(e) if flag.load(Ordering::Relaxed) => Err(e),
                Err(e) => Ok(MultiValue::from_vec(vec![
                    Value::Boolean(false),
                    error_value(lua, &e)?,
                ])),
            }
        })?;
        lua.globals().set("pcall", pcall)?;

        let flag = exceeded;
        let xpcall = lua.create_function(
            move |lua, (f, handler, args): (Function, Function, MultiValue)| match f
                .call::<MultiValue>(args)
            {
                Ok(mut values) => {
                    values.push_front(Value::Boolean(true));
                    Ok(values)
                }
                Err(e) if flag.load(Ordering::Relaxed) => Err(e),
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

    /// Run `f` with instruction/time metering reset and enforced
    fn run_metered<R>(&mut self, f: impl FnOnce(&Lua) -> LuaResult<R>) -> Result<R, SandboxError> {
        // Reset counters for this execution
        self.instruction_count.store(0, Ordering::Relaxed);
        self.exceeded.store(false, Ordering::Relaxed);
        self.metering.reset();
        if let Some(mut budget) = self.lua.app_data_mut::<CallBudget>() {
            budget.db_queries = 0;
            budget.venice_calls = 0;
        }
        self.start_time = Some(Instant::now());

        // Compile and execute
        let result = f(&self.lua);

        // Update metering
        let instr = self.instruction_count.load(Ordering::Relaxed);
        self.metering.add_instructions(instr);
        self.metering.set_memory(self.lua.used_memory() as u64);

        // Check if we exceeded limits
        if self.exceeded.load(Ordering::Relaxed) {
            return Err(SandboxError::InstructionLimitExceeded(
                instr,
                self.config.max_instructions,
            ));
        }

        // Check timeout
        if let Some(start) = self.start_time {
            if start.elapsed() > self.config.timeout {
                return Err(SandboxError::Timeout(self.config.timeout));
            }
        }

        result.map_err(SandboxError::from)
    }

    /// Execute a Lua function with arguments
    pub fn call<A, R>(&mut self, func: Function, args: A) -> Result<R, SandboxError>
    where
        A: mlua::IntoLuaMulti,
        R: mlua::FromLuaMulti,
    {
        self.instruction_count.store(0, Ordering::Relaxed);
        self.exceeded.store(false, Ordering::Relaxed);
        self.start_time = Some(Instant::now());

        let result: LuaResult<R> = func.call(args);

        let instr = self.instruction_count.load(Ordering::Relaxed);
        self.metering.add_instructions(instr);

        if self.exceeded.load(Ordering::Relaxed) {
            return Err(SandboxError::InstructionLimitExceeded(
                instr,
                self.config.max_instructions,
            ));
        }

        result.map_err(SandboxError::from)
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
        self.instruction_count.load(Ordering::Relaxed)
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

#[cfg(test)]
mod tests {
    use super::*;

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
