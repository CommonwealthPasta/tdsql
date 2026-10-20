use super::*;
use crate::value::{DataValue, SqlType};

fn columns() -> Arc<[Column]> {
    Arc::from(vec![Column::new("n", SqlType::Int)])
}

fn row(columns: &Arc<[Column]>, n: i32) -> Result<Streamed> {
    Ok(Streamed::Row(Row::new(
        Arc::clone(columns),
        vec![DataValue::Int(n)],
    )))
}

/// A stream fed by hand, standing in for the connection task.
fn stream(items: Vec<Result<Streamed>>) -> RowStream<'static> {
    let (tx, rx) = mpsc::channel(items.len().max(1));
    for item in items {
        assert!(tx.try_send(item).is_ok(), "channel sized to fit");
    }
    RowStream::new(columns(), rx)
}

#[tokio::test]
async fn hands_over_rows_in_order_then_ends() {
    let cols = columns();
    let mut rows = stream(vec![row(&cols, 1), row(&cols, 2), Ok(Streamed::End)]);

    assert_eq!(rows.columns(), &cols[..]);
    assert_eq!(
        rows.try_next().await.unwrap().unwrap().get::<i32, _>("n"),
        1
    );
    assert_eq!(
        rows.try_next().await.unwrap().unwrap().get::<i32, _>("n"),
        2
    );
    assert!(rows.try_next().await.unwrap().is_none());
}

#[tokio::test]
async fn stays_finished_once_ended() {
    let mut rows = stream(vec![Ok(Streamed::End)]);
    assert!(rows.next().await.is_none());
    assert!(rows.next().await.is_none());
}

#[tokio::test]
async fn an_error_is_the_last_item() {
    let cols = columns();
    let mut rows = stream(vec![row(&cols, 1), Err(Error::Config("boom".into()))]);

    assert!(rows.next().await.unwrap().is_ok());
    assert!(matches!(rows.next().await, Some(Err(Error::Config(_)))));
    assert!(rows.next().await.is_none());
}

#[tokio::test]
async fn hanging_up_without_an_end_is_not_a_complete_result() {
    let cols = columns();
    // One row, then the sender is dropped without `End`.
    let mut rows = stream(vec![row(&cols, 1)]);

    assert!(rows.next().await.unwrap().is_ok());
    assert!(matches!(
        rows.next().await,
        Some(Err(Error::ConnectionClosed))
    ));
    assert!(rows.next().await.is_none());
}
