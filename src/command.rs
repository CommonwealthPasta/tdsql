//! Named-parameter commands: text batches and stored procedures.

use crate::value::DataValue;

/// Whether a [`Command`] is a SQL batch or a stored procedure call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandType {
    /// A literal SQL batch.
    Text,
    /// A stored procedure, invoked by name over RPC.
    StoredProcedure,
}

/// A named parameter and its value.
#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    /// The parameter name, with or without a leading `@`.
    pub name: String,
    /// The bound value.
    pub value: DataValue,
}

impl Parameter {
    /// Bind `value` to `name`.
    ///
    /// ```
    /// use tdsql::Parameter;
    ///
    /// let p = Parameter::new("id", 7);
    /// let missing = Parameter::new("note", None::<String>); // binds NULL
    /// ```
    pub fn new<T: Into<DataValue>>(name: impl Into<String>, value: T) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    /// The name without any leading `@`.
    pub fn bare_name(&self) -> &str {
        self.name.strip_prefix('@').unwrap_or(&self.name)
    }

    /// The name with a leading `@`, which is the form TDS RPC expects.
    pub fn at_name(&self) -> String {
        format!("@{}", self.bare_name())
    }
}

/// A statement plus its named parameters.
///
/// Use this for stored procedures, for named parameters in a text batch, and
/// whenever a command returns more than one result set. For a plain
/// parameterised query, [`Client::query`](crate::Client::query) with positional
/// parameters is shorter.
///
/// ```
/// use tdsql::{Command, Parameter};
///
/// let cmd = Command::stored_procedure("sp_upsert_order")
///     .param("id", 1001)
///     .param("status", "PAID");
///
/// let batch = Command::query("SELECT @id AS id").param("id", 7);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    text: String,
    command_type: CommandType,
    parameters: Vec<Parameter>,
}

/// What a [`Command`] turns into on the wire.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Prepared {
    /// A SQL batch with positional `@P1..@Pn` parameters.
    Text { sql: String, params: Vec<DataValue> },
    /// A stored procedure invoked over RPC with genuinely named parameters.
    Proc {
        name: String,
        params: Vec<(String, DataValue)>,
    },
}

impl Command {
    /// A SQL text batch.
    pub fn query(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            command_type: CommandType::Text,
            parameters: Vec::new(),
        }
    }

    /// A stored procedure, called by name.
    pub fn stored_procedure(name: impl Into<String>) -> Self {
        Self {
            text: name.into(),
            command_type: CommandType::StoredProcedure,
            parameters: Vec::new(),
        }
    }

    /// Bind one named parameter.
    pub fn param<T: Into<DataValue>>(mut self, name: impl Into<String>, value: T) -> Self {
        self.parameters.push(Parameter::new(name, value));
        self
    }

    /// Bind several parameters at once.
    pub fn params<I, N, T>(mut self, params: I) -> Self
    where
        I: IntoIterator<Item = (N, T)>,
        N: Into<String>,
        T: Into<DataValue>,
    {
        self.parameters
            .extend(params.into_iter().map(|(n, v)| Parameter::new(n, v)));
        self
    }

    /// Append an already-built [`Parameter`].
    pub fn with_parameter(mut self, param: Parameter) -> Self {
        self.parameters.push(param);
        self
    }

    /// The SQL text, or the procedure name.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether this is a text batch or a stored procedure.
    pub fn command_type(&self) -> CommandType {
        self.command_type
    }

    /// The bound parameters, in binding order.
    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }

    /// Lower the command to its wire form.
    ///
    /// A stored procedure keeps its named parameters and is sent as an RPC. A
    /// text batch has its `@name` placeholders rewritten to the positional
    /// `@P1..@Pn` the protocol expects.
    pub(crate) fn prepare(&self) -> Prepared {
        match self.command_type {
            CommandType::StoredProcedure => Prepared::Proc {
                name: self.text.clone(),
                // RPC parameter names travel with their `@`; the server
                // matches on the prefixed form.
                params: self
                    .parameters
                    .iter()
                    .map(|p| (p.at_name(), p.value.clone()))
                    .collect(),
            },
            CommandType::Text => {
                let mut sql = self.text.clone();
                for (i, p) in self.parameters.iter().enumerate() {
                    let bare = p.bare_name();
                    // Already positional (`@P1`)? Leave it alone.
                    if is_ordinal_placeholder(bare) {
                        continue;
                    }
                    sql = replace_param_token(&sql, &format!("@{bare}"), &format!("@P{}", i + 1));
                }
                Prepared::Text {
                    sql,
                    params: self.parameters.iter().map(|p| p.value.clone()).collect(),
                }
            }
        }
    }
}

/// Whether `name` is already an ordinal placeholder such as `P1`.
fn is_ordinal_placeholder(name: &str) -> bool {
    match name.strip_prefix('P') {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// The lexical regions of a T-SQL batch. A `@name` token is only a placeholder
/// in `Sql`; anywhere else it is somebody's data or prose and must survive
/// untouched.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    Sql,
    /// `'...'`. A doubled `''` is an escaped quote and stays inside.
    String,
    /// `"..."`, a quoted identifier. A doubled `""` stays inside.
    QuotedIdent,
    /// `[...]`, a bracketed identifier. A doubled `]]` stays inside.
    BracketIdent,
    /// `-- ...`, to the end of the line.
    LineComment,
    /// `/* ... */`, which nests in T-SQL, so the depth is carried.
    BlockComment(u32),
}

/// Characters that continue a T-SQL identifier. `@`, `$` and `#` are all legal
/// inside one, so `@id` must not match inside `@@id`, `@id2` or `@id$x`.
fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '@' || c == '$' || c == '#'
}

/// Replace every occurrence of `needle` that stands alone as an identifier and
/// sits in executable SQL.
///
/// Two things are skipped. Identifier boundaries on *both* sides, so `@id`
/// matches neither `@id2` nor `@@id`. And whole non-SQL regions — string
/// literals, quoted and bracketed identifiers, and comments — so binding `id`
/// no longer rewrites `'@id'` or `'email@id.com'` into the placeholder.
///
/// Copies whole UTF-8 characters, so non-ASCII SQL (`N'café'`) survives intact.
fn replace_param_token(haystack: &str, needle: &str, replacement: &str) -> String {
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    let mut out = String::with_capacity(haystack.len());
    let mut region = Region::Sql;
    // The last character written, for the left-hand identifier boundary.
    let mut prev: Option<char> = None;
    let mut i = 0;

    // Copy a fixed ASCII delimiter and step past it.
    macro_rules! emit {
        ($text:expr) => {{
            out.push_str($text);
            prev = $text.chars().last();
            i += $text.len();
            continue;
        }};
    }

    while i < bytes.len() {
        match region {
            Region::Sql => {
                match bytes[i] {
                    b'\'' => {
                        region = Region::String;
                        emit!("'")
                    }
                    b'"' => {
                        region = Region::QuotedIdent;
                        emit!("\"")
                    }
                    b'[' => {
                        region = Region::BracketIdent;
                        emit!("[")
                    }
                    b'-' if bytes.get(i + 1) == Some(&b'-') => {
                        region = Region::LineComment;
                        emit!("--")
                    }
                    b'/' if bytes.get(i + 1) == Some(&b'*') => {
                        region = Region::BlockComment(1);
                        emit!("/*")
                    }
                    _ => {}
                }

                // `starts_with` comes first: only once it matches is `i + nlen`
                // known to land on a character boundary, so the slice below
                // cannot split a multi-byte character.
                if bytes[i..].starts_with(needle.as_bytes())
                    && prev.is_none_or(|c| !is_ident_continue(c))
                    && haystack[i + nlen..]
                        .chars()
                        .next()
                        .is_none_or(|c| !is_ident_continue(c))
                {
                    out.push_str(replacement);
                    prev = replacement.chars().last();
                    i += nlen;
                    continue;
                }
            }
            Region::String => {
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        emit!("''")
                    }
                    region = Region::Sql;
                }
            }
            Region::QuotedIdent => {
                if bytes[i] == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        emit!("\"\"")
                    }
                    region = Region::Sql;
                }
            }
            Region::BracketIdent => {
                if bytes[i] == b']' {
                    if bytes.get(i + 1) == Some(&b']') {
                        emit!("]]")
                    }
                    region = Region::Sql;
                }
            }
            Region::LineComment => {
                if bytes[i] == b'\n' {
                    region = Region::Sql;
                }
            }
            Region::BlockComment(depth) => {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    region = Region::BlockComment(depth + 1);
                    emit!("/*")
                }
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    region = if depth == 1 {
                        Region::Sql
                    } else {
                        Region::BlockComment(depth - 1)
                    };
                    emit!("*/")
                }
            }
        }

        // Advance by a whole character, never a lone byte.
        let ch = haystack[i..]
            .chars()
            .next()
            .expect("index is on a char boundary");
        out.push(ch);
        prev = Some(ch);
        i += ch.len_utf8();
    }

    out
}

#[cfg(test)]
mod tests;
