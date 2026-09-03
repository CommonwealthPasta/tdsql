//! Integration tests against a live SQL Server.
//!
//! These are `#[ignore]`d by default. Start a server and run them with:
//!
//! ```text
//! docker run -e ACCEPT_EULA=Y -e SA_PASSWORD='YourStrong!Passw0rd' \
//!     -p 1433:1433 -d mcr.microsoft.com/mssql/server:2022-latest
//! cargo test -- --ignored
//! ```

use chrono::{DateTime, NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use uuid::Uuid;

use tdsql::{Client, Command, Config, DataValue, Error, SqlType};

fn test_config() -> Config {
    Config::new()
        .host("localhost")
        .port(1433)
        .database("master")
        .auth("sa", "YourStrong!Passw0rd")
        .trust_cert()
}

/// One connection, reused for every statement in a test.
async fn connect() -> Client {
    Client::connect(&test_config())
        .await
        .expect("failed to connect; is SQL Server running on localhost:1433?")
}

/// DDL runs through the ordinary client now, rather than a hand-rolled driver.
///
/// `CREATE PROCEDURE` and friends must be the first statement of their own
/// batch, so they go through `batch` rather than the parameterised path.
async fn ddl(client: &mut Client, sql: &str) {
    client.batch(sql).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn basic_query() {
    let mut client = connect().await;
    let rows = client.query("SELECT 1 AS value", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>("value"), 1);
}

#[tokio::test]
#[ignore]
async fn bit_query() {
    let mut client = connect().await;
    let row = client
        .query_one("SELECT CAST(1 AS bit) AS value", &[])
        .await
        .unwrap();
    assert!(row.get::<bool, _>("value"));
}

#[tokio::test]
#[ignore]
async fn positional_params() {
    let mut client = connect().await;
    let row = client
        .query_one("SELECT @P1 AS value, @P2 AS flag", &[&7i32, &true])
        .await
        .unwrap();
    assert_eq!(row.get::<i32, _>("value"), 7);
    assert!(row.get::<bool, _>("flag"));
}

#[tokio::test]
#[ignore]
async fn named_params_in_a_text_batch() {
    let mut client = connect().await;
    let cmd = Command::query("SELECT @id AS id, @flag AS flag")
        .param("id", 7)
        .param("flag", false);

    let ds = client.query_dataset(&cmd).await.unwrap();
    assert_eq!(ds[0][0].get::<i32, _>("id"), 7);
    assert!(!ds[0][0].get::<bool, _>("flag"));
}

#[tokio::test]
#[ignore]
async fn non_ascii_sql_survives_parameter_rewriting() {
    let mut client = connect().await;
    let cmd = Command::query("SELECT N'café' AS c, @id AS id").param("id", 1);

    let ds = client.query_dataset(&cmd).await.unwrap();
    assert_eq!(ds[0][0].get::<String, _>("c"), "café");
    assert_eq!(ds[0][0].get::<i32, _>("id"), 1);
}

#[tokio::test]
#[ignore]
async fn stored_procedure_without_params() {
    let mut client = connect().await;
    ddl(
        &mut client,
        "IF OBJECT_ID('sp_no_params', 'P') IS NOT NULL DROP PROCEDURE sp_no_params",
    )
    .await;
    ddl(
        &mut client,
        "CREATE PROCEDURE sp_no_params AS BEGIN SELECT 2 AS value; END",
    )
    .await;

    let ds = client
        .query_dataset(&Command::stored_procedure("sp_no_params"))
        .await
        .unwrap();
    assert_eq!(ds[0][0].get::<i32, _>("value"), 2);
}

#[tokio::test]
#[ignore]
async fn stored_procedure_with_named_params() {
    let mut client = connect().await;
    ddl(
        &mut client,
        "IF OBJECT_ID('sp_with_param', 'P') IS NOT NULL DROP PROCEDURE sp_with_param",
    )
    .await;
    ddl(
        &mut client,
        "CREATE PROCEDURE sp_with_param @val INT AS BEGIN SELECT @val AS value; END",
    )
    .await;

    let cmd = Command::stored_procedure("sp_with_param").param("val", 5);
    let ds = client.query_dataset(&cmd).await.unwrap();
    assert_eq!(ds[0][0].get::<i32, _>("value"), 5);
}

#[tokio::test]
#[ignore]
async fn multiple_result_sets_keep_their_order() {
    let mut client = connect().await;
    let cmd = Command::query("SELECT 1 AS a; SELECT 2 AS b; SELECT 3 AS c");

    let ds = client.query_dataset(&cmd).await.unwrap();
    assert_eq!(ds.len(), 3);
    assert_eq!(ds[0][0].get::<i32, _>("a"), 1);
    assert_eq!(ds[1][0].get::<i32, _>("b"), 2);
    assert_eq!(ds[2][0].get::<i32, _>("c"), 3);
    assert_eq!(ds.table_named("table1").unwrap().len(), 1);
}

#[tokio::test]
#[ignore]
async fn query_one_rejects_zero_and_many_rows() {
    let mut client = connect().await;

    let err = client
        .query_one("SELECT 1 AS v WHERE 1 = 0", &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnexpectedRowCount { found: 0 }),
        "{err}"
    );

    let err = client
        .query_one("SELECT 1 AS v UNION ALL SELECT 2", &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnexpectedRowCount { found: 2 }),
        "{err}"
    );
}

#[tokio::test]
#[ignore]
async fn query_opt_handles_zero_and_one_row() {
    let mut client = connect().await;

    let none = client
        .query_opt("SELECT 1 AS v WHERE 1 = 0", &[])
        .await
        .unwrap();
    assert!(none.is_none());

    let some = client.query_opt("SELECT 1 AS v", &[]).await.unwrap();
    assert_eq!(some.unwrap().get::<i32, _>("v"), 1);
}

#[tokio::test]
#[ignore]
async fn query_scalar_reads_the_first_column() {
    let mut client = connect().await;
    let n: i32 = client.query_scalar("SELECT 42", &[]).await.unwrap();
    assert_eq!(n, 42);

    let s: String = client.query_scalar("SELECT N'hello'", &[]).await.unwrap();
    assert_eq!(s, "hello");
}

#[tokio::test]
#[ignore]
async fn null_round_trips() {
    let mut client = connect().await;

    // Reading a NULL.
    let row = client
        .query_one("SELECT CAST(NULL AS int) AS v", &[])
        .await
        .unwrap();
    assert!(row["v"].is_null());
    assert_eq!(row.try_get::<Option<i32>, _>("v").unwrap(), None);
    assert!(row.try_get::<i32, _>("v").is_err());

    // Binding a NULL.
    let row = client
        .query_one("SELECT @P1 AS v", &[&None::<i32>])
        .await
        .unwrap();
    assert!(row["v"].is_null());
}

#[tokio::test]
#[ignore]
async fn missing_column_is_an_error_not_a_panic() {
    let mut client = connect().await;
    let row = client.query_one("SELECT 1 AS v", &[]).await.unwrap();

    let err = row.try_get::<i32, _>("nope").unwrap_err();
    assert!(
        matches!(err, Error::ColumnNotFound(ref c) if c == "nope"),
        "{err}"
    );
}

#[tokio::test]
#[ignore]
async fn all_types_query() {
    let mut client = connect().await;
    let ds = client
        .query_dataset(&Command::query(
            "SELECT \
                CAST(1 AS tinyint) AS tiny_col, \
                CAST(2 AS smallint) AS small_col, \
                CAST(3 AS int) AS int_col, \
                CAST(4 AS bigint) AS big_col, \
                CAST(5.5 AS float) AS float_col, \
                CAST(123.45 AS numeric(5,2)) AS decimal_col, \
                CAST(1 AS bit) AS bit_col, \
                CAST(N'text' AS nvarchar(10)) AS text_col, \
                CAST(0x010203 AS varbinary(3)) AS binary_col, \
                CAST('6F9619FF-8B86-D011-B42D-00CF4FC964FF' AS uniqueidentifier) AS guid_col, \
                CAST('2023-01-01' AS date) AS date_col, \
                CAST('12:34:56' AS time(0)) AS time_col, \
                CAST('2023-01-01T01:02:03' AS datetime2) AS datetime_col, \
                CAST('2023-01-01T01:02:03+02:00' AS datetimeoffset) AS dto_col, \
                CAST(NULL AS int) AS null_col",
        ))
        .await
        .unwrap();

    let row = &ds[0][0];

    // Typed extraction.
    assert_eq!(row.get::<u8, _>("tiny_col"), 1);
    assert_eq!(row.get::<i16, _>("small_col"), 2);
    assert_eq!(row.get::<i32, _>("int_col"), 3);
    assert_eq!(row.get::<i64, _>("big_col"), 4);
    assert_eq!(row.get::<f64, _>("float_col"), 5.5);
    assert_eq!(row.get::<Decimal, _>("decimal_col"), Decimal::new(12345, 2));
    assert!(row.get::<bool, _>("bit_col"));
    assert_eq!(row.get::<String, _>("text_col"), "text");
    assert_eq!(row.get::<Vec<u8>, _>("binary_col"), vec![1, 2, 3]);
    assert_eq!(
        row.get::<Uuid, _>("guid_col"),
        Uuid::parse_str("6F9619FF-8B86-D011-B42D-00CF4FC964FF").unwrap()
    );
    assert_eq!(
        row.get::<NaiveDate, _>("date_col"),
        NaiveDate::from_ymd_opt(2023, 1, 1).unwrap()
    );
    assert_eq!(
        row.get::<NaiveTime, _>("time_col"),
        NaiveTime::from_hms_opt(12, 34, 56).unwrap()
    );
    assert_eq!(
        row.get::<chrono::NaiveDateTime, _>("datetime_col"),
        NaiveDate::from_ymd_opt(2023, 1, 1)
            .unwrap()
            .and_hms_opt(1, 2, 3)
            .unwrap()
    );
    assert_eq!(
        row.get::<DateTime<chrono::FixedOffset>, _>("dto_col"),
        DateTime::parse_from_rfc3339("2023-01-01T01:02:03+02:00").unwrap()
    );
    assert!(matches!(row["null_col"], DataValue::Null));

    // Integers widen on read.
    assert_eq!(row.get::<i64, _>("tiny_col"), 1);
    assert_eq!(row.get::<i32, _>("small_col"), 2);

    // Column types are a real enum now, not the driver's Debug output.
    let cols = ds[0].columns();
    assert_eq!(cols[0].sql_type(), SqlType::TinyInt);
    assert_eq!(cols[1].sql_type(), SqlType::SmallInt);
    assert_eq!(cols[2].sql_type(), SqlType::Int);
    assert_eq!(cols[3].sql_type(), SqlType::BigInt);
    assert_eq!(cols[4].sql_type(), SqlType::Float);
    assert_eq!(cols[5].sql_type(), SqlType::Numeric);
    assert_eq!(cols[6].sql_type(), SqlType::BitN);
    assert_eq!(cols[7].sql_type(), SqlType::NVarChar);
    assert_eq!(cols[8].sql_type(), SqlType::VarBinary);
    assert_eq!(cols[9].sql_type(), SqlType::Guid);
    assert_eq!(cols[10].sql_type(), SqlType::Date);
    assert_eq!(cols[11].sql_type(), SqlType::Time);
    assert_eq!(cols[12].sql_type(), SqlType::DateTime2);
    assert_eq!(cols[13].sql_type(), SqlType::DateTimeOffset);
    assert_eq!(cols[14].sql_type(), SqlType::Int);

    // Column order is preserved.
    assert_eq!(cols[0].name(), "tiny_col");
    assert_eq!(cols[14].name(), "null_col");
}

#[tokio::test]
#[ignore]
async fn duplicate_column_names_stay_addressable() {
    let mut client = connect().await;
    let row = client
        .query_one("SELECT 1 AS a, 2 AS a", &[])
        .await
        .unwrap();

    assert_eq!(row.len(), 2);
    assert_eq!(row.get::<i32, _>(0), 1);
    assert_eq!(row.get::<i32, _>(1), 2);
}

#[tokio::test]
#[ignore]
async fn execute_reports_rows_affected() {
    let mut client = connect().await;
    ddl(
        &mut client,
        "IF OBJECT_ID('dbo.tdsql_exec_test', 'U') IS NOT NULL DROP TABLE dbo.tdsql_exec_test",
    )
    .await;
    ddl(
        &mut client,
        "CREATE TABLE dbo.tdsql_exec_test (id INT PRIMARY KEY, val INT NOT NULL)",
    )
    .await;

    let affected = client
        .execute(
            "INSERT INTO dbo.tdsql_exec_test (id, val) VALUES (1, 10), (2, 20)",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(affected, 2);

    let affected = client
        .execute(
            "UPDATE dbo.tdsql_exec_test SET val = val + 1 WHERE id = @P1",
            &[&1i32],
        )
        .await
        .unwrap();
    assert_eq!(affected, 1);

    // The same connection keeps working across every statement above.
    let total: i32 = client
        .query_scalar("SELECT COUNT(*) FROM dbo.tdsql_exec_test", &[])
        .await
        .unwrap();
    assert_eq!(total, 2);

    ddl(&mut client, "DROP TABLE dbo.tdsql_exec_test").await;
}

#[tokio::test]
#[ignore]
async fn execute_command_on_a_stored_procedure() {
    let mut client = connect().await;
    ddl(
        &mut client,
        "IF OBJECT_ID('dbo.tdsql_sp_rows', 'U') IS NOT NULL DROP TABLE dbo.tdsql_sp_rows",
    )
    .await;
    ddl(
        &mut client,
        "CREATE TABLE dbo.tdsql_sp_rows (id INT PRIMARY KEY)",
    )
    .await;
    ddl(
        &mut client,
        "IF OBJECT_ID('dbo.sp_tdsql_insert', 'P') IS NOT NULL DROP PROCEDURE dbo.sp_tdsql_insert",
    )
    .await;
    ddl(
        &mut client,
        "CREATE PROCEDURE dbo.sp_tdsql_insert @id INT AS BEGIN \
         INSERT INTO dbo.tdsql_sp_rows (id) VALUES (@id); END",
    )
    .await;

    let cmd = Command::stored_procedure("dbo.sp_tdsql_insert").param("id", 1);
    let affected = client.execute_command(&cmd).await.unwrap();
    assert_eq!(affected, 1);

    ddl(&mut client, "DROP PROCEDURE dbo.sp_tdsql_insert").await;
    ddl(&mut client, "DROP TABLE dbo.tdsql_sp_rows").await;
}

#[tokio::test]
#[ignore]
async fn scalar_from_a_stored_procedure() {
    let mut client = connect().await;
    ddl(
        &mut client,
        "IF OBJECT_ID('dbo.sp_scalar_test', 'P') IS NOT NULL DROP PROCEDURE dbo.sp_scalar_test",
    )
    .await;
    ddl(
        &mut client,
        "CREATE PROCEDURE dbo.sp_scalar_test @x INT AS BEGIN SELECT @x + 1 AS result; END",
    )
    .await;

    let cmd = Command::stored_procedure("dbo.sp_scalar_test").param("x", 5);
    let ds = client.query_dataset(&cmd).await.unwrap();
    assert_eq!(ds[0][0].get::<i32, _>("result"), 6);
}

#[tokio::test]
#[ignore]
async fn connects_from_a_connection_string() {
    let mut client = Client::connect_str(
        "Server=tcp:localhost,1433;Database=master;User Id=sa;\
         Password=YourStrong!Passw0rd;TrustServerCertificate=true",
    )
    .await
    .unwrap();

    let n: i32 = client.query_scalar("SELECT 1", &[]).await.unwrap();
    assert_eq!(n, 1);
}
