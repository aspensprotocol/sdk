//! Collect a server stream for a fixed window that starts only once the
//! stream is established.
//!
//! A snapshot read off a stream (the book, the trader's past trades) is only
//! as good as its window. Counting connection setup against that window lets a
//! slow venue look like an empty book, so the window starts when the RPC has
//! returned its response, and a stream that is not established within
//! [`STREAM_ESTABLISH_TIMEOUT`] is an error, never an empty result.

use std::time::Duration;

use eyre::Result;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// How long a stream may take to be established (connect, then the RPC's
/// response). Matches the gRPC channel's connect timeout.
pub(crate) const STREAM_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(10);

/// A server stream forwarded to a channel by a background task.
pub(crate) struct ForwardedStream<T> {
    /// The stream's items, in order.
    pub rx: mpsc::Receiver<T>,
    /// Fires once the RPC has returned its response: the stream is open.
    pub established: oneshot::Receiver<()>,
    /// The forwarding task: its error, or `Ok` when the stream ended cleanly.
    pub handle: JoinHandle<Result<()>>,
}

/// Aborts the task when dropped; dropping a bare `JoinHandle` detaches it.
pub(crate) struct AbortOnDrop<T>(pub JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The forwarding task's result: its own error, or a join error (panic, abort).
pub(crate) fn finished(
    what: &str,
    res: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    res.map_err(|e| eyre::eyre!("{what} stream task: {e}"))?
        .map_err(|e| e.wrap_err(format!("{what} stream")))
}

/// Wait for the stream to be established. A task that fails first reports its
/// own error (a panic or abort as a join error); a task that ends without
/// establishing, or a stream not
/// established within `timeout`, is an error too. On any error the task is
/// aborted.
pub(crate) async fn await_established(
    what: &str,
    handle: &mut AbortOnDrop<Result<()>>,
    established: oneshot::Receiver<()>,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    tokio::select! {
        // A signal sent before the task ended wins, so a stream that opened
        // and then ended cleanly keeps its items.
        biased;
        signal = established => match signal {
            Ok(()) => Ok(()),
            // The task dropped the sender without firing it, so it has
            // ended or is ending (the sender lives in the task): report how.
            Err(_) => {
                finished(what, (&mut handle.0).await)?;
                Err(eyre::eyre!("{what} stream ended before it was established"))
            }
        },
        _ = &mut deadline => {
            handle.0.abort();
            Err(eyre::eyre!(
                "{what} stream was not established within {timeout:?}; the venue may be slow \
                 or unreachable"
            ))
        }
    }
}

/// Wait (up to `establish_timeout`) for `stream` to be established, then hand
/// each item to `on_item` until `window` has elapsed, then stop the task. The
/// task is also stopped if this future is dropped.
///
/// A stream that fails, or ends without succeeding, before or at the deadline
/// is an error. A stream that ends cleanly keeps what it delivered.
pub(crate) async fn collect_window<T>(
    what: &str,
    stream: ForwardedStream<T>,
    establish_timeout: Duration,
    window: Duration,
    mut on_item: impl FnMut(T),
) -> Result<()> {
    let ForwardedStream {
        mut rx,
        established,
        handle,
    } = stream;
    let mut handle = AbortOnDrop(handle);
    await_established(what, &mut handle, established, establish_timeout).await?;

    // The window starts now that the stream is open.
    let deadline = tokio::time::sleep(window);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            // The task's own outcome first, so a failure at the deadline wins.
            biased;
            res = &mut handle.0 => {
                finished(what, res)?;
                while let Ok(item) = rx.try_recv() {
                    on_item(item);
                }
                return Ok(());
            }
            maybe_item = rx.recv() => match maybe_item {
                Some(item) => on_item(item),
                // The sender is gone, so the task has ended: report how.
                None => return finished(what, (&mut handle.0).await),
            },
            _ = &mut deadline => {
                // The stream never ends on its own; stop it rather than leak it.
                handle.0.abort();
                return Ok(());
            }
        }
    }
}
