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
                    exceeded_clone.store(true, Ordering::Relaxed);
                    Ok(VmState::Yield)
                } else {
                    Ok(VmState::Continue)
                }
            },
        );

        // Remove dangerous globals
        Self::remove_dangerous_globals(&lua)?;

        // Add safe utility functions
        Self::add_safe_globals(&lua)?;

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
        // Reset counters for this execution
        self.instruction_count.store(0, Ordering::Relaxed);
        self.exceeded.store(false, Ordering::Relaxed);
        self.metering.reset();
        self.start_time = Some(Instant::now());

        // Compile and execute
        let chunk = self.lua.load(code);
        let result: LuaResult<R> = chunk.eval();

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
