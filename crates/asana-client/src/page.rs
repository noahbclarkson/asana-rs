//! Paged lists: one page at a time, every page as a stream, or every item as a stream.
//!
//! Asana pages a list with an opaque `offset` that expires, so pages are fetched lazily, in
//! order, as the stream is read, and a failure ends the stream (start again from the beginning).
//! A page with no items also ends it, because some lists (the audit log) always offer a next page.

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

    /// Whether this is the last page.
    pub fn is_last(&self) -> bool {
        self.next.is_none() || self.items.is_empty()
    }
}

impl<T> IntoIterator for Page<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

/// Every item of a list, fetched a page at a time as it is read.
pub type Items<T> = BoxStream<'static, Result<T, Error>>;

/// Every page of a list, fetched as it is read.
pub type Pages<T> = BoxStream<'static, Result<Page<T>, Error>>;

impl Client {
    /// Every page of a list, 100 items to a page, starting at the descriptor's offset (the first
    /// page unless one was set). Nothing is fetched until the stream is read.
    pub fn pages<E: Paginated + 'static>(&self, mut endpoint: E) -> Pages<E::Item> {
        endpoint.set_limit(PageLimit::MAX);
        let client = self.clone();
        stream::unfold(Some(endpoint), move |endpoint| {
            let client = client.clone();
            async move {
                let mut endpoint = endpoint?;
                match client.page_of(&endpoint).await {
                    Ok(page) => {
                        let rest = match (&page.next, page.is_last()) {
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

    /// Every item of a list, fetched 100 at a time as the stream is read.
    pub fn stream<E: Paginated + 'static>(&self, endpoint: E) -> Items<E::Item> {
        self.pages(endpoint)
            .flat_map(|page| match page {
                Ok(page) => Either::Left(stream::iter(page.items).map(Ok)),
                Err(error) => Either::Right(stream::once(async move { Err(error) })),
            })
            .boxed()
    }

    /// The first `max` items of a list, fetching no more pages than that needs.
    ///
    /// # Errors
    ///
    /// The first error any page met, as [`Client::send`].
    pub async fn collect_max<E: Paginated>(
        &self,
        mut endpoint: E,
        max: usize,
    ) -> Result<Vec<E::Item>, Error> {
        let mut items = Vec::new();
        while items.len() < max {
            let wanted = (max - items.len()).min(usize::from(PageLimit::MAX.get()));
            endpoint.set_limit(PageLimit::new(u8::try_from(wanted).unwrap_or(u8::MAX)));
            let page = self.page_of(&endpoint).await?;
            let last = page.is_last();
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
