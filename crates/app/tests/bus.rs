//! Bus behavior tests — multi-subscriber fan-out, slow-subscriber
//! isolation, capacity-Full warning path, receiver-Overflowed recovery.

use std::sync::Arc;
use std::time::Duration;

use async_broadcast::RecvError;
use perima_app::Bus;
use perima_core::{AppEvent, EventBus, FileEvent, MediaPath, VolumeId};

const fn nil_volume() -> VolumeId {
    VolumeId(uuid::Uuid::nil())
}

fn file_event(name: &str) -> AppEvent {
    AppEvent::File(FileEvent::Created {
        path: MediaPath::new(name),
        volume: nil_volume(),
        file_uuid: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_subscriber_fanout() {
    let bus: Arc<Bus> = Bus::new();
    let mut a = bus.subscribe();
    let mut b = bus.subscribe();
    let mut c = bus.subscribe();

    for i in 0..5 {
        bus.emit(&file_event(&format!("f{i}.jpg"))).expect("emit");
    }

    for recv in [&mut a, &mut b, &mut c] {
        for i in 0..5 {
            let event = tokio::time::timeout(Duration::from_secs(1), recv.recv())
                .await
                .expect("recv timeout")
                .expect("recv ok");
            match event {
                AppEvent::File(FileEvent::Created { path, .. }) => {
                    assert_eq!(path.as_str(), &format!("f{i}.jpg"));
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_subscriber_does_not_block_fast() {
    let bus: Arc<Bus> = Bus::new();
    let mut fast = bus.subscribe();
    let mut slow = bus.subscribe();

    // Spawn a slow receiver task that takes 50ms per event.
    let slow_task = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(_event) = slow.recv().await {
            tokio::time::sleep(Duration::from_millis(50)).await;
            count += 1;
            if count >= 10 {
                break;
            }
        }
        count
    });

    // Publish 10 events fast.
    for i in 0..10 {
        bus.emit(&file_event(&format!("f{i}.jpg"))).expect("emit");
    }

    // Fast subscriber should drain all 10 within 200ms (each recv is ~free).
    let start = std::time::Instant::now();
    for _ in 0..10 {
        let _ = tokio::time::timeout(Duration::from_millis(200), fast.recv())
            .await
            .expect("fast recv timeout — slow subscriber blocked us")
            .expect("fast recv ok");
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "fast subscriber drained 10 events in {elapsed:?} — slow blocked us"
    );

    // Slow subscriber eventually completes (10 events × 50ms = 500ms).
    let slow_count = tokio::time::timeout(Duration::from_secs(2), slow_task)
        .await
        .expect("slow task timeout")
        .expect("slow task panic");
    assert_eq!(slow_count, 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_inbox_logs_warns_and_returns_ok() {
    // CAPACITY constant in Bus is 256; we emit 300 without recv to
    // force the Full path. emit must always return Ok — the warning
    // is observable via a tracing subscriber if needed but we just
    // assert no panic + Ok return here.
    let bus: Arc<Bus> = Bus::new();
    let _recv = bus.subscribe(); // hold one receiver so channel is active

    for i in 0..300 {
        let result = bus.emit(&file_event(&format!("f{i}.jpg")));
        assert!(result.is_ok(), "emit returned {result:?} on event {i}");
    }
}

/// In backpressure mode (Bus default, `set_overflow(false)`): senders that exceed
/// capacity get `TrySendError::Full` (which Bus maps to `Ok(())`), and the receiver
/// can drain exactly the messages that fit. No `Overflowed` error in this mode.
///
/// To exercise the `RecvError::Overflowed` path we use a raw channel with overflow
/// mode enabled — this tests the recovery contract we rely on inside the Bus's own
/// receiver logic (the `_inactive` holder), even though Bus itself is backpressure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_overflowed_keeps_going() {
    // WHY raw channel with overflow: Bus is always backpressure mode
    // (set_overflow(false)), so the Overflowed RecvError path is not
    // reachable via Bus::emit. We test the Overflowed recovery directly
    // through a raw async_broadcast channel with overflow enabled.
    let (mut sender, mut recv) = async_broadcast::broadcast::<AppEvent>(256);
    sender.set_overflow(true);

    // Push past capacity — overflow mode bumps oldest, sender succeeds.
    for i in 0..300 {
        sender
            .try_broadcast(file_event(&format!("f{i}.jpg")))
            .expect("overflow-mode send should not fail");
    }
    drop(sender);

    // First recv should return Overflowed indicating we lost events.
    let first = recv.recv().await;
    assert!(
        matches!(first, Err(RecvError::Overflowed(_))),
        "expected Overflowed err, got {first:?}"
    );

    // Subsequent recvs should yield events successfully.
    let mut received = 0;
    while let Ok(_event) = tokio::time::timeout(Duration::from_millis(100), recv.recv())
        .await
        .unwrap_or(Err(RecvError::Closed))
    {
        received += 1;
        if received >= 256 {
            break;
        }
    }
    assert!(
        received >= 200,
        "expected to drain ~256 events post-Overflowed, got {received}"
    );
}

/// Bus is in backpressure mode: emitting 300 events with one subscriber whose
/// inbox is full returns `Ok(())` for every call (the 45 excess are dropped
/// with a `tracing::warn!`). The subscriber drains exactly 256 messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backpressure_mode_drops_excess_and_returns_ok() {
    let bus: Arc<Bus> = Bus::new();
    let mut recv = bus.subscribe();

    // Publish 300 events without draining — first 256 fill the inbox,
    // the rest are dropped silently (Bus logs warn, returns Ok).
    for i in 0..300 {
        let result = bus.emit(&file_event(&format!("f{i}.jpg")));
        assert!(
            result.is_ok(),
            "emit must always return Ok, got {result:?} at {i}"
        );
    }

    // Drain all 256 buffered events.
    let mut received = 0;
    while tokio::time::timeout(Duration::from_millis(50), recv.recv())
        .await
        .unwrap_or(Err(RecvError::Closed))
        .is_ok()
    {
        received += 1;
    }
    assert_eq!(
        received, 256,
        "expected exactly 256 buffered events, got {received}"
    );
}
