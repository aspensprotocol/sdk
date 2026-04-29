//! Synchronous adapters for driving async work from blocking entry
//! points. The CLI uses [`DirectExecutor`] (re-uses the ambient tokio
//! runtime); the REPL owns its own runtime via [`BlockingExecutor`].

use std::future::Future;

/// Run a `Send + 'static` future to completion from a synchronous caller.
pub trait AsyncExecutor {
    /// Block the current thread until `future` resolves and return its output.
    fn execute<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;
}

/// Executor that piggybacks on the caller's existing tokio runtime.
///
/// Use when you're already inside `#[tokio::main]` (CLI entry points).
/// Calls into [`tokio::task::block_in_place`] so it can safely wait on a
/// spawned task without deadlocking the runtime's worker threads.
pub struct DirectExecutor;

impl AsyncExecutor for DirectExecutor {
    fn execute<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // Spawn the future on the current runtime and block until completion.
        // This approach:
        // 1. Spawns the async work on the tokio runtime's worker threads
        // 2. Uses block_in_place to safely block the current thread while waiting
        // This works correctly when called from within an async context
        // (e.g., from #[tokio::main]).
        let handle = tokio::runtime::Handle::current();
        let task = handle.spawn(future);
        tokio::task::block_in_place(move || handle.block_on(task).expect("task panicked"))
    }
}

/// Executor that owns its own multi-threaded tokio runtime.
///
/// Use when there is no ambient runtime (the REPL, scripts, tests). The
/// runtime is created in [`BlockingExecutor::new`] and torn down when
/// the executor is dropped.
pub struct BlockingExecutor {
    rt: tokio::runtime::Runtime,
}

impl Default for BlockingExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockingExecutor {
    /// Build a new multi-threaded tokio runtime with all features enabled.
    pub fn new() -> Self {
        Self {
            rt: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        }
    }
}

impl AsyncExecutor for BlockingExecutor {
    fn execute<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.rt.block_on(future)
    }
}
