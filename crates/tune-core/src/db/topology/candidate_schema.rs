//! Structural compatibility for optional candidate storage; unknown objects are
//! never repaired or written. SQL text is used only for constraints that PRAGMA
//! cannot report, not as an exact DDL fingerprint.
use super::Result;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension};
use std::sync::OnceLock;

type Column = (&'static str, &'static str, i64, Option<&'static str>, i64);
const ENVELOPES: &[Column] =
    &[("scope_id", "TEXT", 1, None, 1), ("envelope", "TEXT", 1, None, 0), ("digest", "TEXT", 1, None, 0), ("ordinal", "INTEGER", 1, None, 0)];
const ROWS: &[Column] = &[
    ("scope_id", "TEXT", 1, None, 1),
    ("row_type", "TEXT", 1, None, 2),
    ("id", "TEXT", 1, None, 3),
    ("kind", "TEXT", 1, None, 0),
    ("search", "TEXT", 1, None, 0),
    ("from_id", "TEXT", 1, None, 0),
    ("to_id", "TEXT", 1, None, 0),
    ("display", "TEXT", 1, None, 0),
    ("digest", "TEXT", 1, None, 0),
];
const LEDGER: &[Column] = &[
    ("scope_id", "TEXT", 1, None, 1),
    ("latest_ordinal", "INTEGER", 1, None, 0),
    ("latest_attempt", "TEXT", 1, None, 0),
    ("latest_status", "TEXT", 1, None, 0),
    ("complete_ordinal", "INTEGER", 1, Some("0"), 0),
];
fn corrupt(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::Utf8Error(..)
    )
}
fn lexer() -> &'static Regex {
    static SQL: OnceLock<Regex> = OnceLock::new();
    SQL.get_or_init(|| {
        Regex::new(r#"(?s)--[^\r\n]*(?:\r?\n|$)|/\*.*?\*/|'(?:''|[^'])*'|"(?:""|[^"])*"|`(?:``|[^`])*`|\[[^\]]*\]|[A-Za-z_][A-Za-z_0-9]*|."#)
            .expect("fixed SQL token pattern")
    })
}
fn comment(token: &str) -> bool {
    token.starts_with("--") || token.starts_with("/*")
}
fn zero_literal(default: &str) -> bool {
    // No SQL evaluation: only a signed integer literal, optional parentheses
    // and comments, is equivalent to the current ledger's integer-zero default.
    let clean = lexer().find_iter(default).map(|m| m.as_str()).filter(|s| !comment(s)).collect::<String>();
    let mut value = clean.trim();
    while value.starts_with('(') && value.ends_with(')') {
        value = value[1..value.len() - 1].trim();
    }
    value.parse::<i64>() == Ok(0)
}
fn supported_constraints(sql: &str) -> bool {
    let mut incomplete_quote = false;
    let tokens = lexer()
        .find_iter(sql)
        .filter_map(|m| {
            let token = m.as_str();
            if comment(token) || token.chars().all(char::is_whitespace) {
                return None;
            }
            let first = token.as_bytes()[0];
            if first.is_ascii_alphabetic() || first == b'_' {
                Some((true, token.to_ascii_uppercase()))
            } else if b"'\"`[".contains(&first) {
                if token.len() < 2 {
                    incomplete_quote = true;
                    return None;
                }
                Some((false, token[1..token.len() - 1].replace("\"\"", "\"").replace("``", "`").replace("''", "'").to_ascii_uppercase()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if incomplete_quote {
        return false;
    }
    for (i, (keyword, value)) in tokens.iter().enumerate() {
        if !keyword {
            continue;
        }
        if ["CHECK", "STRICT"].contains(&value.as_str()) {
            return false;
        }
        if value == "COLLATE" && tokens.get(i + 1).is_none_or(|(_, name)| name != "BINARY") {
            return false;
        }
        if value == "ON"
            && tokens.get(i + 1).is_some_and(|(word, name)| *word && name == "CONFLICT")
            && tokens.get(i + 2).is_none_or(|(word, name)| !*word || name != "ABORT")
        {
            return false;
        }
    }
    true
}
fn table(db: &Connection, name: &str, expected: &[Column]) -> Result<bool> {
    // Bound optional metadata before decoding it. Views and oversized/unknown
    // declarations are unavailable; genuine SQL/IO failures remain errors.
    let sql = db
        .query_row("SELECT sql FROM sqlite_schema WHERE name=? AND type='table' AND typeof(sql)='text' AND length(CAST(sql AS BLOB))<=8192", [name], |r| {
            r.get::<_, String>(0)
        })
        .optional();
    let sql = match sql {
        Ok(Some(sql)) => sql,
        Ok(None) => return Ok(false),
        Err(e) if corrupt(&e) => return Ok(false),
        Err(e) => return Err(e),
    };
    let mode: Option<(i64, i64)> = db
        .query_row("SELECT wr,strict FROM pragma_table_list WHERE schema='main' AND name=? AND type='table'", [name], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    if mode != Some((1, 0)) || db.prepare("SELECT 1 FROM sqlite_schema WHERE type='trigger' AND tbl_name=? LIMIT 1")?.exists([name])? {
        return Ok(false);
    }
    if !supported_constraints(&sql) {
        return Ok(false);
    }
    let mut s = db.prepare("SELECT name,type,\"notnull\",dflt_value,pk,hidden FROM pragma_table_xinfo(?) ORDER BY cid")?;
    let columns = s
        .query_map([name], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?))
        })?
        .collect::<Result<Vec<_>>>();
    let columns = match columns {
        Ok(c) => c,
        Err(e) if corrupt(&e) => return Ok(false),
        Err(e) => return Err(e),
    };
    if columns.len() != expected.len()
        || columns.iter().zip(expected).any(|((n, t, nn, d, pk, hidden), (en, et, enn, ed, epk))| {
            n != en
                || !t.eq_ignore_ascii_case(et)
                || nn != enn
                || pk != epk
                || *hidden != 0
                || match (d, ed) {
                    (None, None) => false,
                    (Some(d), Some("0")) => !zero_literal(d),
                    _ => true,
                }
        })
    {
        return Ok(false);
    }
    let mut s = db.prepare("SELECT 1 FROM pragma_foreign_key_list(?) LIMIT 1")?;
    if s.exists([name])? {
        return Ok(false);
    }
    let mut s = db.prepare("SELECT name,\"unique\",origin,partial FROM pragma_index_list(?)")?;
    let indexes =
        s.query_map([name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))?.collect::<Result<Vec<_>>>();
    let indexes = match indexes {
        Ok(i) => i,
        Err(e) if corrupt(&e) => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut primary = 0;
    let mut key = expected.iter().filter(|c| c.4 > 0).collect::<Vec<_>>();
    key.sort_by_key(|c| c.4);
    for (index, unique, origin, partial) in indexes {
        if unique == 0 {
            continue;
        }
        if unique != 1 || origin != "pk" || partial != 0 {
            return Ok(false);
        }
        primary += 1;
        let mut s = db.prepare("SELECT name,desc,coll FROM pragma_index_xinfo(?) WHERE key=1 ORDER BY seqno")?;
        let columns = s.query_map([index], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>>>();
        let columns = match columns {
            Ok(c) => c,
            Err(e) if corrupt(&e) => return Ok(false),
            Err(e) => return Err(e),
        };
        if columns.len() != key.len() || columns.iter().zip(&key).any(|((n, desc, coll), expected)| n != expected.0 || *desc != 0 || coll != "BINARY") {
            return Ok(false);
        }
    }
    Ok(primary == 1)
}
pub(super) fn ledger(db: &Connection) -> Result<bool> {
    table(db, "docker_candidate_scopes", LEDGER)
}
pub(super) fn projection(db: &Connection) -> Result<bool> {
    Ok(table(db, "docker_ui_envelopes", ENVELOPES)? && table(db, "docker_ui_rows", ROWS)? && ledger(db)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_schema_metadata_matches_fixed_projection_constraints() {
        let db = crate::db::Store::open_in_memory().unwrap();
        assert!(projection(db.conn()).unwrap());
        for (name, spec) in [("docker_ui_envelopes", ENVELOPES), ("docker_ui_rows", ROWS), ("docker_candidate_scopes", LEDGER)] {
            let mode: (i64, i64) =
                db.conn().query_row("SELECT wr,strict FROM pragma_table_list WHERE schema='main' AND name=?", [name], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!(mode, (1, 0));
            let mut s = db.conn().prepare("SELECT name,type,\"notnull\",dflt_value,pk,hidden FROM pragma_table_xinfo(?) ORDER BY cid").unwrap();
            let actual = s
                .query_map([name], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            assert_eq!(actual.len(), spec.len());
            for (actual, expected) in actual.iter().zip(spec) {
                assert_eq!(actual, &(expected.0.into(), expected.1.into(), expected.2, expected.3.map(str::to_owned), expected.4, 0));
            }
            let key = db
                .conn()
                .query_row("SELECT name,\"unique\",partial FROM pragma_index_list(?) WHERE origin='pk'", [name], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
                })
                .unwrap();
            assert_eq!((key.1, key.2), (1, 0));
            let mut s = db.conn().prepare("SELECT name,desc,coll FROM pragma_index_xinfo(?) WHERE key=1 ORDER BY seqno").unwrap();
            let actual = s
                .query_map([key.0], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            let mut expected = spec.iter().filter(|c| c.4 > 0).collect::<Vec<_>>();
            expected.sort_by_key(|c| c.4);
            assert_eq!(actual, expected.iter().map(|c| (c.0.into(), 0, "BINARY".into())).collect::<Vec<_>>());
        }
    }
    #[test]
    fn zero_default_and_sql_constraint_words_are_not_evaluated_or_substring_matched() {
        for value in ["0", "((+0))", "( /* CHECK */ -0)"] {
            assert!(zero_literal(value), "{value}");
        }
        for value in ["0+0", "abs(0)", "'0'", "random()", "(0)+(0)"] {
            assert!(!zero_literal(value), "{value}");
        }
        assert!(supported_constraints(
            "CREATE TABLE t(\"CHECK\" TEXT, x TEXT COLLATE \"BINARY\", CONSTRAINT [CHECK] PRIMARY KEY(x) ON CONFLICT ABORT) /* CHECK */ -- CHECK\n"
        ));
        for ddl in ["CREATE TABLE t(x TEXT CHECK(1))", "CREATE TABLE t(x TEXT COLLATE NOCASE)", "CREATE TABLE t(x TEXT PRIMARY KEY ON CONFLICT IGNORE)"] {
            assert!(!supported_constraints(ddl));
        }
    }
}
