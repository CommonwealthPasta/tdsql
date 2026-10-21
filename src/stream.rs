//! Rows as they arrive, rather than all at once.
//!
//! [`Client::query`](crate::Client::query) waits for the last row before it
//! returns any of them. [`Client::query_stream`](crate::Client::query_stream)
//! hands each row over as soon as the server sends it, so the start of a large
//! result can be shown, forwarded or written out while the rest is still on its
//! way.

use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use futures::{Stream, StreamExt};
use tokio::sync::mpsc;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::row::{Column, Row};

/// What the connection task sends a [`RowStream`] once the columns are known.
///
/// Every stream ends with [`End`](Self::End) or an error, so a channel that
/// closes without either can be told apart from a result that is complete.
pub(crate) enum Streamed {
    Row(Row),
    /// The request ran to completion; nothing follows.
    End,
}

/// The rows of a query's first result set, delivered as the server sends them.
///
/// Created by [`Client::query_stream`](crate::Client::query_stream). It is a
/// [`Stream`] of `Result<Row>`, and [`try_next`](Self::try_next) reads it
/// without importing anything:
///
/// ```no_run
/// # use tdsql::Client;
/// # async fn f(client: &mut Client) -> tdsql::Result<()> {
/// let mut rows = client.query_stream("SELECT id, name FROM users", &[]).await?;
///
/// while let Some(row) = rows.try_next().await? {
///     let id: i32 = row.get("id");
///     println!("{id}");
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Holding the connection
///
/// The stream borrows the client, so nothing else can run on the connection
/// until it is dropped. That is not a formality: the connection serves one
/// request at a time, and while a stream goes unread the connection task is
/// parked waiting for room to hand over the next row. A second statement would
/// queue behind it forever, so the borrow makes that a compile error instead.
///
/// The server is held up too. Rows are read only a little ahead of the
/// consumer, so a slow consumer slows the query down rather than piling rows up
/// in memory -- and for as long as the query is open, it keeps whatever locks
/// it has taken.
///
/// # Dropping early
///
/// Dropping the stream before the end cancels the query on the server, and
/// the connection is ready for the next statement as soon as the server has
/// confirmed. Statements after the `SELECT` in the same batch don't run. See
/// [Cancelling](crate::Client#cancelling) for what can and can't be stopped
/// partway.
pub struct RowStream<'a> {
    columns: Arc<[Column]>,
    rows: mpsc::Receiver<Result<Streamed>>,
    finished: bool,
    _client: PhantomData<&'a mut Client>,
}

impl std::fmt::Debug for RowStream<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowStream")
            .field("columns", &self.columns)
            .field("finished", &self.finished)
            .finish()
    }
}

impl RowStream<'_> {
    pub(crate) fn new(columns: Arc<[Column]>, rows: mpsc::Receiver<Result<Streamed>>) -> Self {
        Self {
            columns,
            rows,
            finished: false,
            _client: PhantomData,
        }
    }

    /// The columns of the result set, known before the first row arrives.
    ///
    /// Empty if the statement produced no result set at all.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The next row, or `None` once the request has finished.
    ///
    /// The same as [`TryStreamExt::try_next`](futures::TryStreamExt::try_next),
    /// without needing `futures` in scope.
    pub async fn try_next(&mut self) -> Result<Option<Row>> {
        self.next().await.transpose()
    }
}

impl Stream for RowStream<'_> {
    type Item = Result<Row>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        let last = match ready!(self.rows.poll_recv(cx)) {
            Some(Ok(Streamed::Row(row))) => return Poll::Ready(Some(Ok(row))),
            Some(Ok(Streamed::End)) => None,
            Some(Err(e)) => Some(Err(e)),
            // The connection task went away without saying the request was
            // over, so the rows so far may not be all of them. Ending quietly
            // here would pass off a truncated result as a complete one.
            None => Some(Err(Error::ConnectionClosed)),
        };
        self.finished = true;
        Poll::Ready(last)
    }
}

#[cfg(test)]
mod tests;
