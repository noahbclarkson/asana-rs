//! [`FakeAsana`]: the handle a test holds, with the controls that are not seeding or acting.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use asana_openapi::endpoint::Endpoint;
use asana_openapi::ids::{Gid, ProjectGid, SectionGid, TaskGid, UserGid, WebhookGid};
use asana_webhooks::{Clock, WebhookSecret, system_clock};
use bytes::Bytes;
use serde_json::Value;

use crate::faults::{Fault, QueuedFault};
use crate::recorded::RecordedRequest;
use crate::select::Selection;
use crate::state::State;
use crate::transport::FakeTransport;
use crate::webhook_delivery::FakeError;

/// An in-memory Asana.
///
/// Cheap to clone: every clone is the same fake. Give a client its [`transport`](Self::transport),
/// seed it with [`workspace`](Self::workspace), act in it as a person would in Asana's web app
/// with [`as_user`](Self::as_user), and look at what was asked with
/// [`requests`](Self::requests).
#[derive(Clone)]
pub struct FakeAsana {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for FakeAsana {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeAsana").finish_non_exhaustive()
    }
}

impl Default for FakeAsana {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeAsana {
    /// An empty fake on the system clock.
    pub fn new() -> Self {
        Self::with_clock(system_clock())
    }

    /// An empty fake whose `created_at`, `modified_at` and event times come from `clock`
    /// (for example an [`asana_webhooks::ManualClock`]).
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::new(clock))),
        }
    }

    /// The state, locked. A panic while it was held leaves it usable: every change is made whole
    /// or not at all before anything that can panic.
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The transport to build a client on:
    /// `Client::builder().transport(fake.transport())`. Any base URL works; the fake reads the
    /// path below `/api/1.0`.
    pub fn transport(&self) -> FakeTransport {
        FakeTransport::new(self.clone())
    }

    /// Every request made so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.lock().requests.clone()
    }

    /// The requests made so far for the operation `E` describes.
    pub fn requests_for<E: Endpoint>(&self) -> Vec<RecordedRequest> {
        self.lock()
            .requests
            .iter()
            .filter(|request| request.is::<E>())
            .cloned()
            .collect()
    }

    /// Forgets the requests recorded so far.
    pub fn clear_requests(&self) {
        self.lock().requests.clear();
    }

    /// Answers the next request for the operation `E` describes with `fault` instead. Queue it
    /// several times for several requests.
    pub fn fail_next<E: Endpoint>(&self, fault: Fault) {
        self.lock().faults.push_back(QueuedFault {
            operation: Some(E::OPERATION_ID),
            fault,
        });
    }

    /// Answers the next request, whatever it is, with `fault` instead.
    pub fn fail_next_request(&self, fault: Fault) {
        self.lock().faults.push_back(QueuedFault {
            operation: None,
            fault,
        });
    }

    /// The user the API's token belongs to (`me`). Unless set with
    /// [`sign_in_as`](Self::sign_in_as) it is the first user seeded, or a user named
    /// "Fake User" made on first need and added to every workspace.
    pub fn me(&self) -> UserGid {
        crate::typed(&self.lock().me())
    }

    /// Makes `user` the token's user.
    pub fn sign_in_as(&self, user: &UserGid) {
        self.lock().me = Some(user.to_string());
    }

    /// Accepts `token` as `user`'s. Once any token is registered, a request with any other token
    /// is answered `401`; until then every bearer token is [`me`](Self::me).
    pub fn accept_token(&self, token: &str, user: &UserGid) {
        self.lock()
            .tokens
            .insert(token.to_owned(), user.to_string());
    }

    /// How many reads of a new job answer `in_progress` before it answers `succeeded`; 0 (the
    /// default) means the first read already succeeds.
    pub fn set_job_polls(&self, polls: u32) {
        self.lock().job_polls = polls;
    }

    /// Forgets every events-stream sync token, so the next call with one is answered `412`, as
    /// Asana answers a token older than a day.
    pub fn expire_sync_tokens(&self) {
        self.lock().sync_tokens.clear();
    }

    /// Registers the receiver that webhook handshakes, deliveries and heartbeats are sent to,
    /// replacing any earlier one. Each call is an `http::Request<Bytes>` whose URI is the
    /// webhook's full target.
    ///
    /// The whole webhook path, with the application's receiver built on
    /// [`asana_webhooks::Receiver`] as a framework adapter would serve it at
    /// `/webhooks/asana/{route}`:
    ///
    /// ```
    /// use asana_client::{Auth, Client};
    /// use asana_fake::FakeAsana;
    /// use asana_webhooks::{EventAction, Filter, MemoryHandshakeStore, MemorySecrets, Receiver};
    /// use bytes::Bytes;
    /// use tokio::sync::mpsc;
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let fake = FakeAsana::new();
    /// let acme = fake.workspace("Acme");
    /// let ada = acme.user("Ada Lovelace", "ada@example.com");
    /// let project = acme.project("Onboarding");
    /// let [todo, done] = project.sections(["To do", "Done"]);
    /// let task = project.task("Welcome Ada").in_section(&todo).gid();
    /// let client = Client::builder()
    ///     .auth(Auth::personal_access_token("0/fake"))
    ///     .transport(fake.transport())
    ///     .build()?;
    ///
    /// // The application's receiver: handshakes into a store, verified batches into a channel.
    /// let (sink, mut deliveries) = mpsc::channel(16);
    /// let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), sink);
    /// let app = receiver.clone();
    /// fake.receiver(move |request: http::Request<Bytes>| {
    ///     let app = app.clone();
    ///     async move {
    ///         let route = request.uri().path().rsplit('/').next().unwrap_or_default().to_owned();
    ///         let headers: Vec<(String, String)> = request
    ///             .headers()
    ///             .iter()
    ///             .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
    ///             .collect();
    ///         let reply = app.receive(&route, &headers, request.body()).await;
    ///         let mut response = http::Response::builder().status(reply.status());
    ///         if let Some(echo) = reply.echo() {
    ///             response = response.header(echo.header_name(), echo.header_value());
    ///         }
    ///         response.body(Bytes::new()).unwrap_or_default()
    ///     }
    /// });
    ///
    /// // The fake handshakes into the receiver while `establish` waits.
    /// let established = client
    ///     .webhooks()
    ///     .establish(project.gid(), "https://hooks.example.com/webhooks/asana")
    ///     .filter(Filter::task().action(EventAction::Added))
    ///     .store(receiver.handshakes())
    ///     .send()
    ///     .await?;
    /// receiver.secrets().insert(&established.route, established.secret);
    ///
    /// // Someone drags the task to "Done"; the fake signs and delivers the event.
    /// fake.as_user(&ada).move_task(&task, &done);
    /// let delivered = fake.deliver().await;
    /// assert_eq!(delivered[0].status, http::StatusCode::OK);
    ///
    /// let delivery = deliveries.try_recv()?;
    /// assert_eq!(delivery.batch.events[0].section_entered(), Some((task, done)));
    /// # Ok(())
    /// # }
    /// ```
    pub fn receiver<F, Fut>(&self, receiver: F)
    where
        F: Fn(http::Request<Bytes>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = http::Response<Bytes>> + Send + 'static,
    {
        self.lock().receiver = Some(Arc::new(move |request| Box::pin(receiver(request))));
    }

    /// Whether every API request delivers the events waiting at its end before it is answered.
    /// Off by default: call [`deliver`](Self::deliver).
    pub fn set_auto_deliver(&self, on: bool) {
        self.lock().auto_deliver = on;
    }

    /// Creates a webhook on `resource` delivering to `target` without going through a client:
    /// the same checks and the same handshake against the [`receiver`](Self::receiver) as
    /// `POST /webhooks`.
    ///
    /// # Errors
    ///
    /// [`FakeError::Refused`] with what Asana would answer: an unknown resource, a target that is
    /// not `https` or is local, a workspace webhook without filters, or a receiver that did not
    /// echo the secret.
    pub async fn create_webhook<G: Gid>(
        &self,
        resource: &G,
        target: &str,
    ) -> Result<WebhookGid, FakeError> {
        self.create_webhook_with_filters(resource, target, Vec::new())
            .await
    }

    /// [`create_webhook`](Self::create_webhook) with filters, each a JSON object
    /// `{resource_type, resource_subtype, action, fields}` (an `asana_webhooks::Filter`
    /// serialises to one).
    ///
    /// # Errors
    ///
    /// As [`create_webhook`](Self::create_webhook).
    pub async fn create_webhook_with_filters<G: Gid>(
        &self,
        resource: &G,
        target: &str,
        filters: Vec<Value>,
    ) -> Result<WebhookGid, FakeError> {
        let (gid, _) = self.establish(resource.as_str(), target, filters).await?;
        Ok(crate::typed(&gid))
    }

    /// The secret that signs `webhook`'s deliveries.
    pub fn webhook_secret(&self, webhook: &WebhookGid) -> Option<WebhookSecret> {
        self.lock()
            .webhooks
            .get(webhook.as_str())
            .map(|webhook| WebhookSecret::from_bytes(webhook.secret.as_bytes().to_vec()))
    }

    /// The events waiting to be delivered to `webhook`, as JSON in Asana's shape.
    pub fn pending_events(&self, webhook: &WebhookGid) -> Vec<Value> {
        self.lock()
            .webhooks
            .get(webhook.as_str())
            .map(|webhook| webhook.pending.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The object `gid` names as a `GET` without `opt_fields` answers it (its full record), or
    /// `None` when it does not exist. For asserting on state without a client.
    pub fn record<G: Gid>(&self, gid: &G) -> Option<Value> {
        let state = self.lock();
        state
            .exists(gid.as_str())
            .then(|| Selection::default().one(&state, gid.as_str()))
    }

    /// The section `task` is in within `project`, if it is in the project and in a section.
    pub fn section_of(&self, task: &TaskGid, project: &ProjectGid) -> Option<SectionGid> {
        let state = self.lock();
        let section = state
            .tasks
            .get(task.as_str())?
            .memberships
            .iter()
            .find(|membership| membership.project == project.as_str())?
            .section
            .clone()?;
        Some(crate::typed(&section))
    }
}
