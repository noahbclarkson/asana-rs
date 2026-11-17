//! What is sent again, when, and what a caller sees when retries run out.

use std::time::Duration;

use asana_client::{Error, Limits, RetryPolicy, TransportError};

use crate::support::{
    AddTag, CreateThing, Scripted, client, client_with, get_task, reply, reply_with, task_body,
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
async fn a_retry_after_longer_than_the_policy_waits_fails_at_once_but_still_cools_the_client() {
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
    client.send(get_task()).await.unwrap();
    assert_eq!(transport.times()[1] - start, Duration::from_secs(120));
}
