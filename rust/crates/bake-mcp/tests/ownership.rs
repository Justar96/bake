//! Option refusal before any work starts, and ownership of the client's task
//! through initialization, including initialization that is cancelled.
//!
//! The transport halves are `Tracked` pipes, so "the task released the
//! transport" is observed directly rather than inferred from return values.

mod common;

use std::num::NonZeroUsize;
use std::time::Duration;

use bake_mcp::{ClientOptions, CloseReason, ConnectError, McpClient, OptionsError};
use common::{Wire, bounded, init_result, options, wire};
use tokio::sync::Semaphore;
use tokio::time::Instant;

/// Poll `future` once, so it reaches its first wait.
async fn poll_once<F: Future + Unpin>(future: &mut F) {
    tokio::select! {
        biased;
        _ = future => panic!("finished on its first poll"),
        () = std::future::ready(()) => {}
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// `edit` applied to the defaults is refused naming `field`, and nothing was
/// started: both halves are already dropped and the server read nothing.
async fn refused(edit: impl FnOnce(&mut ClientOptions), field: &str) -> OptionsError {
    let mut options = options();
    edit(&mut options);
    let Wire {
        receiver,
        sender,
        mut server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64 * 1024, 1 << 20);
    let error = match McpClient::start(receiver, sender, options) {
        Err(ConnectError::InvalidOptions(error)) => error,
        Err(other) => panic!("expected refused options, got {other}"),
        Ok(_) => panic!("{field}: out-of-range options were accepted"),
    };
    assert!(error.to_string().starts_with(field), "{error}");
    assert!(!receiver_alive.held() && !sender_alive.held());
    server.expect_eof().await;
    error
}

/// `edit` applied to the defaults connects and shuts down normally.
async fn accepted(edit: impl FnOnce(&mut ClientOptions)) {
    let mut options = options();
    edit(&mut options);
    let (client, _events, _server, receiver_alive, sender_alive) =
        common::connect_wire(wire(64 * 1024, 1 << 20), options, init_result()).await;
    assert_eq!(
        bounded("shutdown", client.shutdown()).await,
        CloseReason::Shutdown
    );
    receiver_alive.released().await;
    sender_alive.released().await;
}

#[tokio::test]
async fn capacities_beyond_tokio_are_refused_before_anything_starts() {
    // Before validation, these panicked in Tokio's semaphore ("a semaphore
    // may not have more than MAX_PERMITS permits"); see the review evidence.
    let too_large = NonZeroUsize::new(Semaphore::MAX_PERMITS + 1).unwrap();
    for (field, set) in [
        (
            "limits.max_pending_requests",
            (|o, v| o.limits.max_pending_requests = v) as fn(&mut ClientOptions, NonZeroUsize),
        ),
        ("limits.max_events", |o, v| o.limits.max_events = v),
    ] {
        for value in [too_large, NonZeroUsize::MAX] {
            let error = refused(|o| set(o, value), field).await;
            assert!(
                matches!(error, OptionsError::CapacityTooLarge { value: v, max, .. }
                if v == value.get() && max == Semaphore::MAX_PERMITS)
            );
        }
        // Control: Tokio's largest capacity itself is accepted.
        accepted(|o| set(o, NonZeroUsize::new(Semaphore::MAX_PERMITS).unwrap())).await;
    }
    // Thresholds that size no channel take any value.
    accepted(|o| {
        o.limits.max_queued_messages = NonZeroUsize::MAX;
        o.limits.max_list_pages = NonZeroUsize::MAX;
        o.limits.max_listed_tools = NonZeroUsize::MAX;
        o.limits.max_list_bytes = NonZeroUsize::MAX;
        o.limits.max_message_bytes = NonZeroUsize::MAX;
    })
    .await;
}

#[tokio::test]
async fn timeouts_the_clock_cannot_represent_are_refused_before_anything_starts() {
    // Before validation, `request_timeout: Some(Duration::MAX)` panicked in
    // `connect` ("overflow when adding duration to instant"), and
    // `shutdown_timeout: Duration::MAX` panicked inside the driver task at
    // shutdown, which still reported a clean `Shutdown`.
    let error = refused(
        |o| o.request_timeout = Some(Duration::MAX),
        "request_timeout",
    )
    .await;
    assert_eq!(
        error,
        OptionsError::DurationTooLong {
            field: "request_timeout",
            value: Duration::MAX
        }
    );
    let error = refused(|o| o.shutdown_timeout = Duration::MAX, "shutdown_timeout").await;
    assert!(matches!(
        error,
        OptionsError::DurationTooLong {
            field: "shutdown_timeout",
            ..
        }
    ));
    // Control: long timeouts the clock can represent are accepted, and so is
    // no request timeout at all.
    let long = Duration::from_secs(u64::MAX / 4);
    accepted(|o| {
        o.request_timeout = Some(long);
        o.shutdown_timeout = long;
    })
    .await;
    accepted(|o| o.request_timeout = None).await;
}

#[test]
fn options_can_be_checked_without_a_runtime() {
    let mut options = options();
    assert_eq!(options.validate(), Ok(()));
    options.limits.max_events = NonZeroUsize::MAX;
    assert!(options.validate().is_err());
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_initialize_leaves_the_task_owned_until_shutdown() {
    let Wire {
        receiver,
        sender,
        mut server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64 * 1024, 1 << 20);
    let mut connecting = McpClient::start(receiver, sender, options()).unwrap();
    {
        let mut initializing = Box::pin(connecting.initialize());
        tokio::select! {
            _ = &mut initializing => panic!("initialized without an answer"),
            _ = server.request("initialize") => {}
        }
        // Cancelled: the initialize future is dropped unanswered.
    }
    // The task is still running and still holds the transport: its owner,
    // not a detached task, decides when it ends.
    settle().await;
    assert!(receiver_alive.held() && sender_alive.held());
    // `initialize` cannot be sent twice on one connection: the second call
    // is refused, and the server never reads a second request.
    let second = bounded("a second initialize", async {
        tokio::select! {
            second = connecting.initialize() => second,
            message = server.recv() => panic!("initialize was sent twice: {message}"),
        }
    })
    .await;
    assert!(matches!(second, Err(ConnectError::AlreadyAttempted)));
    assert_eq!(
        bounded("shutdown", connecting.shutdown()).await,
        Some(CloseReason::Shutdown)
    );
    // Released by the time `shutdown` returned, and `initialize` was never
    // cancelled on the wire.
    assert!(!receiver_alive.held() && !sender_alive.held());
    server.expect_eof().await;
    // Repeated shutdown still waits for nothing and reports the same reason.
    assert_eq!(
        bounded("shutdown", connecting.shutdown()).await,
        Some(CloseReason::Shutdown)
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_after_a_cancelled_initialize_waits_for_the_transport() {
    // The client-to-server pipe holds 64 bytes and the server never reads, so
    // the initialize request's write stays in flight: the task needs the full
    // shutdown deadline before it gives the transport up.
    let mut options = options();
    options.shutdown_timeout = Duration::from_secs(2);
    let Wire {
        receiver,
        sender,
        server: _server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64, 1 << 20);
    let mut connecting = McpClient::start(receiver, sender, options).unwrap();
    {
        let mut initializing = Box::pin(connecting.initialize());
        poll_once(&mut initializing).await;
        settle().await;
    }
    let started = Instant::now();
    assert_eq!(
        bounded("shutdown", connecting.shutdown()).await,
        Some(CloseReason::Shutdown)
    );
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(2), "{elapsed:?}");
    assert!(!receiver_alive.held() && !sender_alive.held());
}

#[tokio::test(start_paused = true)]
async fn a_dropped_connect_leaves_the_task_without_an_owner() {
    // The weaker convenience path, documented on `McpClient::connect`: the
    // same stuck write, but nothing can wait for the task once the future is
    // gone. It still holds the transport after the drop and lets it go only
    // when its shutdown deadline passes.
    let mut options = options();
    options.shutdown_timeout = Duration::from_secs(2);
    let Wire {
        receiver,
        sender,
        server: _server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64, 1 << 20);
    {
        let mut connecting = Box::pin(McpClient::connect(receiver, sender, options));
        poll_once(&mut connecting).await;
        settle().await;
    }
    settle().await;
    assert!(receiver_alive.held() && sender_alive.held());
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!receiver_alive.held() && !sender_alive.held());
}

#[tokio::test]
async fn a_successful_initialize_hands_the_task_to_the_client() {
    let Wire {
        receiver,
        sender,
        mut server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64 * 1024, 1 << 20);
    let mut connecting = McpClient::start(receiver, sender, options()).unwrap();
    let (initialized, _) = bounded("the handshake", async {
        tokio::join!(connecting.initialize(), server.handshake(init_result()))
    })
    .await;
    let (client, _events) = initialized.unwrap();
    // The `Connecting` no longer owns the task.
    assert_eq!(bounded("shutdown", connecting.shutdown()).await, None);
    assert!(matches!(
        bounded("a second initialize", connecting.initialize()).await,
        Err(ConnectError::AlreadyAttempted)
    ));
    assert!(receiver_alive.held() && sender_alive.held());
    assert_eq!(
        bounded("shutdown", client.shutdown()).await,
        CloseReason::Shutdown
    );
    assert!(!receiver_alive.held() && !sender_alive.held());
}

#[tokio::test]
async fn a_failed_initialize_has_released_the_transport() {
    let Wire {
        receiver,
        sender,
        mut server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64 * 1024, 1 << 20);
    let mut connecting = McpClient::start(receiver, sender, options()).unwrap();
    let (initialized, ()) = bounded("the handshake", async {
        tokio::join!(connecting.initialize(), async {
            let (id, _) = server.request("initialize").await;
            server
                .reply(&id, serde_json::json!({"protocolVersion": "1999-01-01"}))
                .await;
        })
    })
    .await;
    assert!(matches!(initialized, Err(ConnectError::Initialize(_))));
    assert!(!receiver_alive.held() && !sender_alive.held());
    assert_eq!(
        bounded("shutdown", connecting.shutdown()).await,
        Some(CloseReason::Shutdown)
    );
}
