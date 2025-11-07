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
        let handle = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(future)
        });
        futures::executor::block_on(handle).unwrap()
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
