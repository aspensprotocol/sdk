//! The orderbook stream must close once nobody reads it.
//!
//! The `Orderbook` RPC never ends on its own — live updates follow the
//! snapshot — so a client that stops reading but keeps the stream open holds a
//! server subscription for the life of the process. `fetch_top_of_book` once
//! did exactly that on every call.
//!
//! These tests stand up a minimal gRPC server over raw HTTP/2 (the generated
//! bindings have no server stubs) and watch for the client resetting the
//! stream. Each covers one of the two ways the stream used to leak:
//!
//! - a quiet book: the snapshot arrives, then nothing. With no further entry to
//!   fail a send, only aborting the forwarding task closes the stream.
//! - a busy book whose receiver is dropped: entries keep arriving, so the
//!   forwarding task must stop at the first send that finds no receiver.

#![cfg(all(feature = "client", feature = "trader"))]

use std::future::poll_fn;
use std::time::Duration;

use aspens::commands::trading::arborter_pb::{OrderState, OrderbookEntry, Side};
use aspens::commands::trading::stream_orderbook::{
    StreamOrderbookOptions, fetch_top_of_book, stream_orderbook_channel,
};
use bytes::Bytes;
use prost::Message;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// How long the client gets to reset the stream before we call it leaked.
/// Generous: the reset is immediate when it happens at all.
const CLOSE_DEADLINE: Duration = Duration::from_secs(5);

/// What the fake server does after the snapshot entry.
#[derive(Clone, Copy)]
enum AfterSnapshot {
    /// Send nothing more — a market with no activity.
    GoQuiet,
    /// Keep sending an update every few milliseconds.
    KeepStreaming,
}

/// One Confirmed bid at a fixed price, so the caller can check it was read.
fn bid_entry() -> OrderbookEntry {
    OrderbookEntry {
        order_id: vec![7; 32],
        price: "100".into(),
        quantity: "1".into(),
        side: Side::Bid as i32,
        market_id: "m".into(),
        state: OrderState::Confirmed as i32,
        ..Default::default()
    }
}

/// A gRPC length-prefixed message: uncompressed flag, big-endian length, body.
fn grpc_frame(entry: &OrderbookEntry) -> Bytes {
    let body = entry.encode_to_vec();
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.push(0);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Bytes::from(frame)
}

/// Start a fake `Orderbook` server. Returns its URL and a channel that
/// receives `()` each time a client resets (closes) its stream.
async fn fake_server(after: AfterSnapshot) -> (String, mpsc::UnboundedReceiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (closed_tx, closed_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let closed_tx = closed_tx.clone();
            tokio::spawn(async move {
                let mut conn = h2::server::handshake(socket).await.unwrap();
                // `accept` also drives the connection's I/O, so keep polling it.
                while let Some(Ok((_request, mut respond))) = conn.accept().await {
                    let closed_tx = closed_tx.clone();
                    tokio::spawn(async move {
                        let response = http::Response::builder()
                            .status(200)
                            .header("content-type", "application/grpc")
                            .body(())
                            .unwrap();
                        let mut send = respond.send_response(response, false).unwrap();
                        let frame = grpc_frame(&bid_entry());
                        if send.send_data(frame.clone(), false).is_err() {
                            let _ = closed_tx.send(());
                            return;
                        }
                        loop {
                            let tick = async {
                                match after {
                                    AfterSnapshot::GoQuiet => std::future::pending().await,
                                    AfterSnapshot::KeepStreaming => {
                                        tokio::time::sleep(Duration::from_millis(10)).await
                                    }
                                }
                            };
                            tokio::select! {
                                // The client reset the stream: it was closed.
                                _ = poll_fn(|cx| send.poll_reset(cx)) => {
                                    let _ = closed_tx.send(());
                                    return;
                                }
                                _ = tick => {
                                    if send.send_data(frame.clone(), false).is_err() {
                                        let _ = closed_tx.send(());
                                        return;
                                    }
                                }
                            }
                        }
                    });
                }
            });
        }
    });

    (url, closed_rx)
}

/// Wait for the server to see the stream closed, or fail as a leak.
async fn expect_closed(closed: &mut mpsc::UnboundedReceiver<()>, what: &str) {
    match tokio::time::timeout(CLOSE_DEADLINE, closed.recv()).await {
        Ok(Some(())) => {}
        Ok(None) => panic!("{what}: fake server stopped unexpectedly"),
        Err(_) => panic!(
            "{what}: the orderbook stream was still open {CLOSE_DEADLINE:?} after the caller \
             stopped reading — it leaked"
        ),
    }
}

/// `fetch_top_of_book` returns after its window; the stream must close with
/// it, even when the market is quiet and no further entry would ever fail a
/// send.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_top_of_book_closes_its_stream_on_a_quiet_book() {
    let (url, mut closed) = fake_server(AfterSnapshot::GoQuiet).await;

    let top = fetch_top_of_book(url, "m".into(), Duration::from_millis(300))
        .await
        .expect("fetch_top_of_book");
    // The fake server was really read — the close below isn't from a stream
    // that never worked.
    assert_eq!(top.best_bid, Some(100));

    expect_closed(&mut closed, "fetch_top_of_book").await;
}

/// Dropping the receiver of `stream_orderbook_channel` must end the
/// forwarding task (and close the stream) at the next entry, without the
/// caller having to abort the handle.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_channel_receiver_ends_a_busy_stream() {
    let (url, mut closed) = fake_server(AfterSnapshot::KeepStreaming).await;

    let (mut rx, handle) = stream_orderbook_channel(
        url,
        StreamOrderbookOptions {
            market_id: "m".into(),
            historical_open_orders: true,
            filter_by_trader: None,
        },
    )
    .await
    .expect("subscribe");
    let first = tokio::time::timeout(CLOSE_DEADLINE, rx.recv())
        .await
        .expect("first entry in time")
        .expect("an entry");
    assert_eq!(first.price, "100");

    drop(rx);

    expect_closed(&mut closed, "stream_orderbook_channel").await;
    let ended = tokio::time::timeout(CLOSE_DEADLINE, handle)
        .await
        .expect("forwarding task ended")
        .expect("forwarding task did not panic");
    assert!(
        ended.is_ok(),
        "a dropped receiver is a clean stop: {ended:?}"
    );
}
