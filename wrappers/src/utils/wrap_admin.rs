use anyhow::Result;
use std::future::Future;
use tracing::info;

#[cfg(feature = "admin")]
use aspens::commands::config::{self, config_pb};

use crate::utils::executor::AsyncExecutor;

#[cfg(feature = "admin")]
pub fn wrap_get_config<E: AsyncExecutor>(executor: &E, url: String) -> Result<config_pb::Config> {
    info!("Fetching config from {url}");
    let result = executor.execute(config::call_get_config(url))?;
    info!("GetConfig result: {result:?}");
    Ok(result)
}

#[cfg(feature = "admin")]
pub fn wrap_download_config<E: AsyncExecutor>(
    executor: &E,
    url: String,
    path: String,
) -> Result<()> {
    info!("Downloading config from {url} to {path}");
    let result = executor.execute(config::download_config_to_file(url, path))?;
    info!("DownloadConfig result: {result:?}");
    Ok(())
}
