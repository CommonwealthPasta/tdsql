use super::*;

fn text_sql(cmd: &Command) -> String {
    match cmd.prepare() {
        Prepared::Text { sql, .. } => sql,
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn rewrites_named_params_to_positional() {
    let cmd = Command::query("SELECT @id AS id, @flag AS flag")
        .param("id", 7)
        .param("flag", false);
    assert_eq!(text_sql(&cmd), "SELECT @P1 AS id, @P2 AS flag");
}

#[test]
fn respects_identifier_boundaries() {
    // `@id` must not match inside `@id2`.
    let cmd = Command::query("SELECT @id, @id2")
        .param("id", 1)
        .param("id2", 2);
    assert_eq!(text_sql(&cmd), "SELECT @P1, @P2");
}

#[test]
fn accepts_at_prefixed_names() {
    let cmd = Command::query("SELECT @when").param("@when", 1);
    assert_eq!(text_sql(&cmd), "SELECT @P1");
}

#[test]
fn leaves_existing_ordinals_alone() {
    let cmd = Command::query("SELECT @P1").param("P1", 5);
    assert_eq!(text_sql(&cmd), "SELECT @P1");
}

#[test]
fn preserves_non_ascii_sql() {
    // Regression: the previous rewriter cast each byte to a `char`, turning
    // multi-byte UTF-8 into mojibake.
    let cmd = Command::query("SELECT N'café', N'日本語', @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT N'café', N'日本語', @P1");
}

#[test]
fn preserves_non_ascii_with_no_params() {
    let cmd = Command::query("SELECT N'Ñoño — café'");
    assert_eq!(text_sql(&cmd), "SELECT N'Ñoño — café'");
}

#[test]
fn stored_procedure_keeps_named_params_for_rpc() {
    let cmd = Command::stored_procedure("sp_upsert")
        .param("id", 1001)
        .param("@status", "PAID");

    match cmd.prepare() {
        Prepared::Proc { name, params } => {
            assert_eq!(name, "sp_upsert");
            // RPC parameter names keep the `@`, however they were written.
            assert_eq!(params[0].0, "@id");
            assert_eq!(params[1].0, "@status");
            assert_eq!(params[1].1, DataValue::Text("PAID".into()));
        }
        other => panic!("expected proc, got {other:?}"),
    }
}

#[test]
fn params_binds_several_at_once() {
    let cmd = Command::query("SELECT @a, @b").params([("a", 1), ("b", 2)]);
    assert_eq!(cmd.parameters().len(), 2);
    assert_eq!(text_sql(&cmd), "SELECT @P1, @P2");
}

#[test]
fn binds_null_from_none() {
    let cmd = Command::query("SELECT @note").param("note", None::<String>);
    assert_eq!(cmd.parameters()[0].value, DataValue::Null);
}

#[test]
fn bare_name_strips_at() {
    assert_eq!(Parameter::new("@id", 1).bare_name(), "id");
    assert_eq!(Parameter::new("id", 1).bare_name(), "id");
}

#[test]
fn at_name_adds_exactly_one_at() {
    assert_eq!(Parameter::new("@id", 1).at_name(), "@id");
    assert_eq!(Parameter::new("id", 1).at_name(), "@id");
}

#[test]
fn ordinal_placeholder_detection() {
    assert!(is_ordinal_placeholder("P1"));
    assert!(is_ordinal_placeholder("P42"));
    assert!(!is_ordinal_placeholder("P"));
    assert!(!is_ordinal_placeholder("Price"));
    assert!(!is_ordinal_placeholder("id"));
}
