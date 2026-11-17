//! What is sent again, when, and what a caller sees when retries run out.

use std::time::Duration;

use asana_client::{Error, Limits, Outcome, RetryPolicy, TransportError};

use crate::support::{
    AddTag, CreateThing, DeleteThing, Scripted, client, client_with, get_task, reply, reply_with,
    task_body,
};

fn too_many(seconds: &str) -> http::Response<bytes::Bytes> {
    reply_with(
        429,
        ("retry-after", seconds),
        r#"{"errors":[{"message":"Rate limited"}]}"#,
    )
}

#[tokio::test(start_paused = true)]
async fn a_429_waits_exactly_retry_after_and_holds_back_a_concurrent_request() {
    let transport = Scripted::new(|_, index| {
        Ok(if index == 0 {
            too_many("7")
        } else {
            reply(200, task_body())
        })
    });
    let client = client(transport.clone());
    let start = tokio::time::Instant::now();

    let first = tokio::spawn({
        let client = client.clone();
        async move { client.send(get_task()).await }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let second = tokio::spawn({
        let client = client.clone();
        async move { client.send(get_task()).await }
    });
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    let offsets: Vec<_> = transport.times().iter().map(|at| *at - start).collect();
    assert_eq!(offsets.len(), 3);
    assert_eq!(offsets[0], Duration::ZERO);
    // The retry and the request started during the cooldown both go out when it ends, not before.
    assert_eq!(offsets[1], Duration::from_secs(7));
    assert_eq!(offsets[2], Duration::from_secs(7));
}

#[tokio::test(start_paused = true)]
async fn a_post_that_creates_is_retried_on_429_but_not_on_503() {
    let transport = Scripted::replies(vec![Ok(too_many("2")), Ok(reply(503, "{}"))]);
    let client = client(transport.clone());
    let error = client
        .send(CreateThing {
            name: "Acme".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Server { status, attempts: 2, .. } if status == 503),
        "{error:?}"
    );
    assert_eq!(transport.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_get_is_retried_on_503_with_back_off() {
    let transport = Scripted::replies(vec![
        Ok(reply(503, "{}")),
        Ok(reply(502, "{}")),
        Ok(reply(200, task_body())),
    ]);
    let client = client(transport.clone());
    let task = client.send(get_task()).await.unwrap();
    assert_eq!(task.name.as_deref(), Some("Acme launch"));
    let times = transport.times();
    assert_eq!(times.len(), 3);
    let first_wait = times[1] - times[0];
    let second_wait = times[2] - times[1];
    assert!(first_wait >= Duration::from_millis(250) && first_wait <= Duration::from_millis(500));
    assert!(second_wait >= Duration::from_millis(500) && second_wait <= Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn a_post_marked_idempotent_is_retried_on_a_server_failure() {
    let transport = Scripted::replies(vec![Ok(reply(500, "{}")), Ok(reply(200, r#"{"data":{}}"#))]);
    let client = client(transport.clone());
    client.send(AddTag).await.unwrap();
    assert_eq!(transport.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_post_is_retried_after_a_connection_that_never_reached_asana_but_not_after_a_timeout() {
    let transport = Scripted::replies(vec![
        Err(TransportError::connect("refused")),
        Ok(reply(201, r#"{"data":{"gid":"1"}}"#)),
    ]);
    client(transport.clone())
        .send(CreateThing {
            name: "Acme".into(),
        })
        .await
        .unwrap();
    assert_eq!(transport.count(), 2);

    let transport = Scripted::replies(vec![
        Err(TransportError::timeout("slow")),
        Ok(reply(201, r#"{"data":{"gid":"1"}}"#)),
    ]);
    let error = client(transport.clone())
        .send(CreateThing {
            name: "Acme".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Transport { attempts: 1, .. }),
        "{error:?}"
    );
    assert_eq!(transport.count(), 1);

    let transport = Scripted::replies(vec![
        Err(TransportError::timeout("slow")),
        Ok(reply(200, task_body())),
    ]);
    client(transport.clone()).send(get_task()).await.unwrap();
    assert_eq!(transport.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn exhausted_retries_say_how_many_attempts_were_made() {
    let transport = Scripted::replies(vec![Ok(too_many("1"))]);
    let error = client(transport.clone())
        .send(get_task())
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::RateLimited { attempts: 5, retry_after } if retry_after == Duration::from_secs(1)),
        "{error:?}"
    );
    assert_eq!(transport.count(), 5);
    assert!(error.to_string().contains("attempts: 5"));

    let transport = Scripted::replies(vec![Ok(reply(503, "{}"))]);
    let policy = RetryPolicy::builder().max_attempts(3).build();
    let error = client_with(transport.clone(), policy, Limits::default())
        .send(get_task())
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Server { attempts: 3, .. }),
        "{error:?}"
    );
    assert!(error.is_retryable());
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_longer_than_the_policy_waits_fails_at_once_and_so_do_other_calls() {
    let transport = Scripted::new(|_, index| {
        Ok(if index == 0 {
            too_many("120")
        } else {
            reply(200, task_body())
        })
    });
    let client = client(transport.clone());
    let start = tokio::time::Instant::now();
    let error = client.send(get_task()).await.unwrap_err();
    assert!(
        matches!(error, Error::RateLimited { attempts: 1, .. }),
        "{error:?}"
    );

    // Another call meets the client's cooldown: two minutes is over `max_wait` (a minute), so it
    // fails at once, unsent, with how long is left.
    tokio::time::sleep(Duration::from_secs(10)).await;
    let error = client.send(get_task()).await.unwrap_err();
    assert!(
        matches!(error, Error::RateLimited { attempts: 0, retry_after } if retry_after == Duration::from_secs(110)),
        "{error:?}"
    );
    assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(10));
    assert_eq!(transport.count(), 1);

    // Once less than `max_wait` is left, a call waits it out.
    tokio::time::sleep(Duration::from_secs(70)).await;
    client.send(get_task()).await.unwrap();
    assert_eq!(transport.times()[1] - start, Duration::from_secs(120));
}

#[tokio::test(start_paused = true)]
async fn max_wait_bounds_every_wait_for_a_rate_limit() {
    // With the guide's 30 seconds, a `Retry-After: 45` fails the call that saw it, and every call
    // started during the cooldown fails at once instead of hanging for 45 seconds.
    let transport = Scripted::new(|_, index| {
        Ok(if index == 0 {
            too_many("45")
        } else {
            reply(200, task_body())
        })
    });
    let policy = RetryPolicy::builder()
        .max_wait(Duration::from_secs(30))
        .build();
    let client = client_with(transport.clone(), policy, Limits::default());
    let start = tokio::time::Instant::now();
    assert!(client.send(get_task()).await.is_err());

    let waiting: Vec<_> = (0..3)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.send(get_task()).await })
        })
        .collect();
    for call in waiting {
        let error = call.await.unwrap().unwrap_err();
        assert!(
            matches!(error, Error::RateLimited { attempts: 0, .. }),
            "{error:?}"
        );
        // The caller is invited to try again later; doing so at once fails at once again.
        assert!(error.is_retryable());
    }
    assert_eq!(tokio::time::Instant::now(), start);
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_policy_that_never_retries_never_waits_for_a_rate_limit_either() {
    let transport = Scripted::new(|_, index| {
        Ok(if index == 0 {
            too_many("5")
        } else {
            reply(200, task_body())
        })
    });
    let client = client_with(transport.clone(), RetryPolicy::never(), Limits::default());
    let start = tokio::time::Instant::now();
    assert!(matches!(
        client.send(get_task()).await,
        Err(Error::RateLimited { attempts: 1, .. })
    ));
    assert!(matches!(
        client.send(get_task()).await,
        Err(Error::RateLimited { attempts: 0, .. })
    ));
    assert_eq!(tokio::time::Instant::now(), start);

    tokio::time::sleep(Duration::from_secs(5)).await;
    client.send(get_task()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_too_large_for_the_clock_does_not_panic() {
    let transport = Scripted::replies(vec![Ok(too_many("18446744073709551615"))]);
    let client = client(transport.clone());
    let error = client.send(get_task()).await.unwrap_err();
    assert!(
        matches!(error, Error::RateLimited { attempts: 1, retry_after } if retry_after == Duration::from_secs(24 * 60 * 60)),
        "{error:?}"
    );
    // The client is cooled down for a day, not for ever, and says so without sending.
    let error = client.send(get_task()).await.unwrap_err();
    assert!(
        matches!(error, Error::RateLimited { attempts: 0, .. }),
        "{error:?}"
    );
    assert_eq!(transport.count(), 1);
}

fn create() -> CreateThing {
    CreateThing {
        name: "Acme".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_create_whose_outcome_is_unknown_is_never_called_retryable() {
    for failure in [
        Err(TransportError::timeout("slow")),
        Err(TransportError::other("connection reset")),
        Ok(reply(503, "{}")),
        Ok(reply(500, "{}")),
    ] {
        let transport = Scripted::replies(vec![failure]);
        let error = client(transport.clone()).send(create()).await.unwrap_err();
        assert_eq!(error.outcome(), Outcome::Unknown, "{error:?}");
        assert!(!error.is_retryable(), "{error:?}");
        assert!(
            matches!(
                error,
                Error::Transport {
                    safe_to_repeat: false,
                    ..
                } | Error::Server {
                    safe_to_repeat: false,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(transport.count(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn not_sent_refused_and_unknown_are_told_apart() {
    // Never reached Asana: safe to send again, whatever it does.
    let policy = RetryPolicy::builder().max_attempts(1).build();
    let transport = Scripted::replies(vec![Err(TransportError::connect("refused"))]);
    let error = client_with(transport, policy, Limits::default())
        .send(create())
        .await
        .unwrap_err();
    assert_eq!(error.outcome(), Outcome::NotSent);
    assert!(error.is_retryable());

    // Refused: Asana did nothing.
    let transport = Scripted::replies(vec![Ok(reply(400, "{}"))]);
    let error = client(transport).send(create()).await.unwrap_err();
    assert_eq!(error.outcome(), Outcome::Refused);
    assert!(!error.is_retryable());

    // A read whose outcome is unknown is still worth trying again: reading twice is harmless.
    let transport = Scripted::new(|_, _| Err(TransportError::timeout("slow")));
    let error = client(transport.clone())
        .send(get_task())
        .await
        .unwrap_err();
    assert_eq!(transport.count(), 5);
    assert_eq!(error.outcome(), Outcome::Unknown);
    assert!(error.is_retryable());

    // Applied: the answer came but could not be read.
    let transport = Scripted::replies(vec![Ok(reply(201, r#"{"data":{"gid":7}}"#))]);
    let error = client(transport).send(create()).await.unwrap_err();
    assert_eq!(error.outcome(), Outcome::Applied, "{error:?}");
}

#[tokio::test(start_paused = true)]
async fn a_delete_sent_again_after_a_lost_answer_and_answered_404_counts_as_deleted() {
    for lost in [Err(TransportError::timeout("slow")), Ok(reply(503, "{}"))] {
        let transport = Scripted::replies(vec![lost, Ok(reply(404, "{}"))]);
        client(transport.clone()).send(DeleteThing).await.unwrap();
        assert_eq!(transport.count(), 2);
    }

    // A 404 on the first attempt is still the caller's to see.
    let transport = Scripted::replies(vec![Ok(reply(404, "{}"))]);
    let error = client(transport).send(DeleteThing).await.unwrap_err();
    assert!(error.is_not_found(), "{error:?}");

    // And so is one after an attempt that never reached Asana.
    let transport = Scripted::replies(vec![
        Err(TransportError::connect("refused")),
        Ok(reply(404, "{}")),
    ]);
    let error = client(transport).send(DeleteThing).await.unwrap_err();
    assert!(error.is_not_found(), "{error:?}");
}
