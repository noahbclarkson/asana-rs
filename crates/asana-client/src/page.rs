//! Paged lists: one page at a time, every page as a stream, or every item as a stream.
//!
//! Asana pages a list with an opaque `offset` that expires, so pages are fetched lazily, in
//! order, as the stream is read, and a failure ends the stream (start again from the beginning).
//! A list ends where Asana offers no next page. The audit log is the exception: once it has
//! offered a next page it always does, to stream future events, so for it a page with no items
//! also ends the list.

use asana_openapi::endpoint::{Offset, PageLimit, Paginated};
use futures_util::future::Either;
use futures_util::stream::{self, BoxStream, StreamExt};

use crate::client::Client;
use crate::error::Error;

/// One page of a list, and where the next one starts.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Page<T> {
    /// The page's items, in Asana's order.
    pub items: Vec<T>,
    /// The offset of the next page, or `None` on the last page.
    pub next: Option<Offset>,
}

impl<T> Page<T> {
    /// A page of `items`, followed by the page at `next`.
    pub fn new(items: Vec<T>, next: Option<Offset>) -> Self {
        Self { items, next }
    }

    /// Whether this is the last page: Asana offered no next one.
    ///
    /// A page with no items can still have a next page, and the list goes on there. The audit
    /// log (`getAuditLogEvents`) always offers a next page once it has offered one;
    /// [`Client::pages`] and [`Client::stream`] end it at its first page with no items.
    pub fn is_last(&self) -> bool {
        self.next.is_none()
    }
}

impl<T> IntoIterator for Page<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

/// The lists that offer a next page even when they have nothing more for now, so that their
/// first page with no items is where they end.
const ENDLESS_LISTS: &[&str] = &["getAuditLogEvents"];

/// Whether `page` is where the list `E` ends.
fn ends_list<E: Paginated, T>(page: &Page<T>) -> bool {
    page.is_last() || (page.items.is_empty() && ENDLESS_LISTS.contains(&E::OPERATION_ID))
}

/// Every item of a list, fetched a page at a time as it is read.
pub type Items<T> = BoxStream<'static, Result<T, Error>>;

/// Every page of a list, fetched as it is read.
pub type Pages<T> = BoxStream<'static, Result<Page<T>, Error>>;

impl Client {
    /// Every page of a list, starting at the descriptor's offset (the first page unless one was
    /// set). Pages hold the descriptor's `limit`, or 100 items when it sets none. Nothing is
    /// fetched until the stream is read.
    pub fn pages<E: Paginated + 'static>(&self, mut endpoint: E) -> Pages<E::Item> {
        if endpoint.limit().is_none() {
            endpoint.set_limit(PageLimit::MAX);
        }
        let client = self.clone();
        stream::unfold(Some(endpoint), move |endpoint| {
            let client = client.clone();
            async move {
                let mut endpoint = endpoint?;
                match client.page_of(&endpoint).await {
                    Ok(page) => {
                        let rest = match (&page.next, ends_list::<E, _>(&page)) {
                            (Some(next), false) => {
                                endpoint.set_offset(Some(next.clone()));
                                Some(endpoint)
                            }
                            _ => None,
                        };
                        Some((Ok(page), rest))
                    }
                    Err(error) => Some((Err(error), None)),
                }
            }
        })
        .boxed()
    }

    /// Every item of a list, fetched a page at a time as the stream is read: the descriptor's
    /// `limit` to a page, or 100 when it sets none.
    pub fn stream<E: Paginated + 'static>(&self, endpoint: E) -> Items<E::Item> {
        self.pages(endpoint)
            .flat_map(|page| match page {
                Ok(page) => Either::Left(stream::iter(page.items).map(Ok)),
                Err(error) => Either::Right(stream::once(async move { Err(error) })),
            })
            .boxed()
    }

    /// The first `max` items of a list, fetching no more pages than that needs, and pages no
    /// larger than the descriptor's `limit` when it sets one.
    ///
    /// # Errors
    ///
    /// The first error any page met, as [`Client::send`].
    pub async fn collect_max<E: Paginated>(
        &self,
        mut endpoint: E,
        max: usize,
    ) -> Result<Vec<E::Item>, Error> {
        let page_size = usize::from(endpoint.limit().unwrap_or(PageLimit::MAX).get());
        let mut items = Vec::new();
        while items.len() < max {
            let wanted = (max - items.len()).min(page_size);
            endpoint.set_limit(PageLimit::new(u8::try_from(wanted).unwrap_or(u8::MAX)));
            let page = self.page_of(&endpoint).await?;
            let last = ends_list::<E, _>(&page);
            let next = page.next.clone();
            items.extend(page.items);
            match next {
                Some(next) if !last => endpoint.set_offset(Some(next)),
                _ => break,
            }
        }
        items.truncate(max);
        Ok(items)
    }
}
