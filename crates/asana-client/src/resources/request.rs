//! The request every resource handle returns: a generated descriptor bound to a client.
//!
//! A handle method such as `client.tasks().get(task)` builds the generated descriptor and wraps it
//! in a [`Request`]. The resource's own module adds the setters that make sense for that operation
//! (`.fields(..)`, `.name(..)`, `.due_on(..)`) as inherent methods on `Request<'_, ThatDescriptor>`,
//! and every request ends the same way: `.send().await` for one call, and `.stream()`, `.pages()`
//! or `.collect_max(n)` for a list.

use asana_openapi::endpoint::{Endpoint, Paginated};

use crate::{Client, Error, Items, Page, Pages};

/// One operation ready to send: a generated descriptor and the client that will send it.
///
/// Nothing is sent until [`send`](Self::send) (or, for a list, [`stream`](Self::stream),
/// [`pages`](Self::pages), [`collect_max`](Self::collect_max) or
/// [`first_page`](Self::first_page)) is called. Limits, retries and authentication are the
/// client's, exactly as for [`Client::send`].
#[must_use = "a request does nothing until it is sent"]
#[derive(Debug)]
pub struct Request<'a, E> {
    client: &'a Client,
    endpoint: E,
}

impl<'a, E: Endpoint> Request<'a, E> {
    /// Binds a descriptor to a client.
    pub fn new(client: &'a Client, endpoint: E) -> Self {
        Self { client, endpoint }
    }

    /// The descriptor that will be sent.
    pub fn endpoint(&self) -> &E {
        &self.endpoint
    }

    /// The descriptor, for setting a parameter no setter covers.
    pub fn endpoint_mut(&mut self) -> &mut E {
        &mut self.endpoint
    }

    /// Changes the descriptor in place and hands the request back, for a parameter no setter
    /// covers: `.map(|e| e.data.liked = Some(true))`.
    pub fn map(mut self, change: impl FnOnce(&mut E)) -> Self {
        change(&mut self.endpoint);
        self
    }

    /// The descriptor alone, for example to put it in a batch.
    pub fn into_endpoint(self) -> E {
        self.endpoint
    }

    /// Sends the request once and returns its result.
    ///
    /// For a list this is one page, as the descriptor asks for it; [`stream`](Self::stream) reads
    /// them all.
    ///
    /// # Errors
    ///
    /// As [`Client::send`].
    pub async fn send(self) -> Result<E::Output, Error> {
        self.client.send(self.endpoint).await
    }
}

impl<E: Paginated + 'static> Request<'_, E> {
    /// Every item of the list, fetched 100 at a time as the stream is read.
    pub fn stream(self) -> Items<E::Item> {
        self.client.stream(self.endpoint)
    }

    /// Every page of the list, 100 items to a page.
    pub fn pages(self) -> Pages<E::Item> {
        self.client.pages(self.endpoint)
    }

    /// The first `max` items, fetching no more pages than that needs.
    ///
    /// # Errors
    ///
    /// The first error any page met, as [`Client::send`].
    pub async fn collect_max(self, max: usize) -> Result<Vec<E::Item>, Error> {
        self.client.collect_max(self.endpoint, max).await
    }

    /// One page, with the offset of the next, as the descriptor's `limit` and `offset` ask.
    ///
    /// # Errors
    ///
    /// As [`Client::send`].
    pub async fn first_page(self) -> Result<Page<E::Item>, Error> {
        self.client.send_page(self.endpoint).await
    }
}

/// Adds `.fields(..)` to the requests for descriptors that take `opt_fields`.
///
/// `opt_fields_setter!(GetTask => TaskField, GetSection => SectionField);` in a resource module
/// gives `Request<'_, GetTask>` a `fields(Fields<TaskField>)` method, so every handle chooses
/// fields the same way: `.fields(fields![Task: name, due_on])`.
macro_rules! opt_fields_setter {
    ($($endpoint:ty => $field:ty),+ $(,)?) => { $(
        impl $crate::resources::request::Request<'_, $endpoint> {
            /// Chooses the fields Asana returns (`opt_fields`), for example
            /// `fields![Task: name, due_on]`. Without it Asana returns its compact record.
            pub fn fields(
                mut self,
                fields: ::asana_openapi::fields::Fields<$field>,
            ) -> Self {
                self.endpoint_mut().opt_fields = fields;
                self
            }
        }
    )+ };
}
pub(crate) use opt_fields_setter;
