//! The per-client concurrency limits and the search bucket.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asana_client::{Auth, Client, Limits, Transport, TransportError};
use bytes::Bytes;
use futures_util::future::{BoxFuture, join_all};

use crate::support::{BASE, CreateThing, Scripted, SearchTasks, client_with, get_task, reply};
use asana_client::RetryPolicy;

/// Holds each request for a second and records the most in flight at once, per method.
#[derive(Default)]
struct Counting {
    state: Mutex<(HashMap<http::Method, usize>, HashMap<http::Method, usize>)>,
}

impl Counting {
    fn most(&self, method: &http::Method) -> usize {
        self.state
            .lock()
            .unwrap()
            .1
            .get(method)
            .copied()
            .unwrap_or(0)
    }
}

impl Transport for Counting {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async move {
            let method = request.method().clone();
            {
                let mut state = self.state.lock().unwrap();
                let now = {
                    let current = state.0.entry(method.clone()).or_default();
                    *current += 1;
                    *current
                };
                let most = state.1.entry(method.clone()).or_default();
                *most = (*most).max(now);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            *self.state.lock().unwrap().0.get_mut(&method).unwrap() -= 1;
            let body = if method == http::Method::GET {
                crate::support::task_body()
            } else {
                r#"{"data":{"gid":"1"}}"#
            };
            Ok(reply(200, body))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn reads_and_writes_are_each_held_to_their_own_limit() {
    let transport = Arc::new(Counting::default());
    let client = Client::builder()
        .auth(Auth::personal_access_token("0/acme-test-token-not-real"))
        .transport(transport.clone())
        .base_url(BASE)
        .limits(Limits::builder().reads(3).writes(2).build())
        .build()
        .unwrap();
    let start = tokio::time::Instant::now();
    let reads = (0..9).map(|_| {
        let client = client.clone();
        tokio::spawn(async move { client.send(get_task()).await.map(|_| ()) })
    });
    let writes = (0..6).map(|_| {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .send(CreateThing {
                    name: "Acme".into(),
                })
                .await
                .map(|_| ())
        })
    });
    let all: Vec<_> = reads.chain(writes).collect();
    for result in join_all(all).await {
        result.unwrap().unwrap();
    }
    assert_eq!(transport.most(&http::Method::GET), 3);
    assert_eq!(transport.most(&http::Method::POST), 2);
    // Nine reads three at a time, and six writes two at a time, each take three seconds.
    assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(3));
}

#[tokio::test(start_paused = true)]
async fn workspace_searches_are_spread_over_the_minute() {
    let transport = Scripted::new(|_, _| Ok(reply(200, r#"{"data":[]}"#)));
    let client = client_with(
        transport.clone(),
        RetryPolicy::default(),
        Limits::builder().searches_per_minute(2).build(),
    );
    let start = tokio::time::Instant::now();
    for _ in 0..3 {
        client.send(SearchTasks).await.unwrap();
    }
    client.send(get_task()).await.ok();
    let offsets: Vec<_> = transport.times().iter().map(|at| *at - start).collect();
    assert_eq!(offsets[0], Duration::ZERO);
    assert_eq!(offsets[1], Duration::ZERO);
    assert!(offsets[2] >= Duration::from_secs(29) && offsets[2] <= Duration::from_secs(31));
    // Other operations do not wait for the search bucket.
    assert_eq!(offsets[3], offsets[2]);
}
