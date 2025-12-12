use std::future::Future;

pub trait AsyncExecutor {
    fn execute<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;
}

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
        tokio::task::block_in_place(move || {
            handle.block_on(task).expect("task panicked")
        })
    }
}

pub struct BlockingExecutor {
    rt: tokio::runtime::Runtime,
}

impl Default for BlockingExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockingExecutor {
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
