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
fn leaves_string_literals_alone() {
    // Regression: `'@id'` and `'email@id.com'` used to be rewritten into the
    // placeholder, silently corrupting the literal.
    let cmd =
        Command::query("SELECT @id AS bound, '@id' AS lit, 'email@id.com' AS addr").param("id", 7);
    assert_eq!(
        text_sql(&cmd),
        "SELECT @P1 AS bound, '@id' AS lit, 'email@id.com' AS addr"
    );
}

#[test]
fn handles_doubled_quotes_inside_a_literal() {
    // The `''` is an escaped quote, so the literal does not end there and the
    // `@id` after it is still data.
    let cmd = Command::query("SELECT 'it''s @id', @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT 'it''s @id', @P1");
}

#[test]
fn leaves_quoted_and_bracketed_identifiers_alone() {
    let cmd = Command::query(r#"SELECT [@id], "@id", @id"#).param("id", 1);
    assert_eq!(text_sql(&cmd), r#"SELECT [@id], "@id", @P1"#);
}

#[test]
fn handles_doubled_delimiters_inside_identifiers() {
    let cmd = Command::query(r#"SELECT [a]]@id], @id"#).param("id", 1);
    assert_eq!(text_sql(&cmd), r#"SELECT [a]]@id], @P1"#);
}

#[test]
fn leaves_comments_alone() {
    let cmd = Command::query("SELECT @id -- pass @id here\n, @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT @P1 -- pass @id here\n, @P1");
}

#[test]
fn leaves_block_comments_alone() {
    let cmd = Command::query("SELECT /* @id */ @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT /* @id */ @P1");
}

#[test]
fn block_comments_nest() {
    // T-SQL nests block comments, so the first `*/` does not end the outer one.
    let cmd = Command::query("SELECT /* a /* @id */ @id */ @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT /* a /* @id */ @id */ @P1");
}

#[test]
fn respects_the_left_identifier_boundary() {
    // Regression: `@id` used to match inside `@@id`, which is how T-SQL spells
    // a global variable such as `@@ROWCOUNT`.
    let cmd = Command::query("SELECT @@id, @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT @@id, @P1");
}

#[test]
fn treats_dollar_and_hash_as_identifier_characters() {
    // Both are legal inside a T-SQL identifier, so neither token is `@id`.
    let cmd = Command::query("SELECT @id$x, @id#y, @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT @id$x, @id#y, @P1");
}

#[test]
fn rewrites_after_a_literal_containing_a_quote() {
    // The scanner has to leave the literal in the right state, or every
    // placeholder after it is missed.
    let cmd = Command::query("SELECT 'a''b', @id, 'c', @flag")
        .param("id", 1)
        .param("flag", 2);
    assert_eq!(text_sql(&cmd), "SELECT 'a''b', @P1, 'c', @P2");
}

#[test]
fn non_ascii_inside_a_literal_does_not_split() {
    // The needle length must never be used to index into the middle of a
    // multi-byte character.
    let cmd = Command::query("SELECT N'café @id 日本語', @id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT N'café @id 日本語', @P1");
}

#[test]
fn an_unterminated_literal_swallows_the_rest() {
    // Malformed SQL is the server's problem, but the rewriter must not panic
    // or produce something different from what the caller wrote.
    let cmd = Command::query("SELECT '@id").param("id", 1);
    assert_eq!(text_sql(&cmd), "SELECT '@id");
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
