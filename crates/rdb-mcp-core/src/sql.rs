//! Classification of SQL text: read/write detection and syntax validation.
//!
//! Pure functions over the query string — no connection, no I/O. They decide how a
//! statement's response is shaped, never whether it is allowed to run.

/// Replaces the contents of string literals, quoted identifiers, and comments with
/// a single space, so that classification can scan the remainder without being
/// fooled by quoted text.
///
/// Masked spans are `'...'`, `"..."`, and `` `...` `` (a doubled delimiter escapes
/// itself and continues the span), `-- ...` to the end of the line, and `/* ... */`.
/// Nested block comments are not supported; the first `*/` closes the span.
///
/// Known conservative gaps, both accepted: backslash escapes inside MySQL string
/// literals (`'a\'b'`) and PostgreSQL dollar-quoted strings (`$$...$$`) are not
/// understood, so such queries are reported as unterminated. Callers surface that
/// as a request error rather than running the statement and discarding its result.
///
/// # Errors
/// Returns a static message when a comment or quoted span never closes.
fn mask_sql_literals_and_comments(sql: &str) -> Result<String, &'static str> {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    // Start of the current run of characters that need no masking.
    let mut plain_start = 0;

    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1).copied()) {
            (b'-', Some(b'-')) => {
                out.push_str(&sql[plain_start..i]);
                out.push(' ');
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                // The newline itself survives as a token separator.
                plain_start = i;
            }
            (b'/', Some(b'*')) => {
                out.push_str(&sql[plain_start..i]);
                out.push(' ');
                i += 2;
                loop {
                    if i + 1 >= bytes.len() {
                        return Err("unterminated block comment (/* without closing */)");
                    }
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                plain_start = i;
            }
            (quote @ (b'\'' | b'"' | b'`'), _) => {
                out.push_str(&sql[plain_start..i]);
                out.push(' ');
                i += 1;
                loop {
                    if i >= bytes.len() {
                        return Err("unterminated quoted string or identifier");
                    }
                    if bytes[i] == quote {
                        if bytes.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                plain_start = i;
            }
            // Advancing one byte through a multi-byte character is safe: every
            // delimiter above is ASCII, so a slice boundary never lands mid-character.
            _ => i += 1,
        }
    }
    out.push_str(&sql[plain_start..]);
    Ok(out)
}

/// Checks the query for syntactic issues that would cause misclassification.
///
/// Detects comments and quoted spans that never close. Call this before
/// [`is_read_query`] to get a proper error for malformed queries; `is_read_query`
/// conservatively returns `false` on scan failure.
///
/// # Errors
/// Returns a static message describing the syntactic defect that would prevent
/// reliable classification.
pub fn validate_query_syntax(query: &str) -> Result<(), &'static str> {
    mask_sql_literals_and_comments(query)?;
    Ok(())
}

/// Checks that a keyword at position `0..prefix_len` is followed by a word boundary
/// (whitespace, `(`, or end of string).
///
/// # Panics
/// Panics if `prefix_len > s.len()`. Callers must ensure
/// `s.starts_with(keyword)` before calling with `keyword.len()`.
fn is_keyword_at_boundary(s: &str, prefix_len: usize) -> bool {
    debug_assert!(prefix_len <= s.len(), "prefix_len exceeds string length");
    if s.len() == prefix_len {
        return true;
    }
    let next = s.as_bytes()[prefix_len];
    next.is_ascii_whitespace() || next == b'('
}

/// Determines whether a SQL query returns a result set, based on its leading keyword.
///
/// This selects the response shape only — a result set for `true`, an affected-row
/// count for `false`. It is not a permission check and never prevents a statement
/// from running: the `execute_sql` tool is meant to run arbitrary SQL, writes
/// included. The cost of a wrong `false` is therefore a lost result set, not a
/// blocked query, so the classification errs toward whatever keeps that rare.
///
/// String literals, quoted identifiers, and comments are masked out first (see
/// [`mask_sql_literals_and_comments`]), so quoted semicolons and quoted keywords
/// no longer skew the decision. What remains is judged by:
///
/// - an embedded semicolon (after trailing ones are dropped) — treated as
///   multi-statement input, which cannot be reported as a single result set;
/// - a leading `SELECT`, `SHOW`, `PRAGMA`, `DESCRIBE`, or `EXPLAIN` token;
/// - `WITH`, where a DML keyword anywhere in the statement means the CTE writes.
///
/// `EXPLAIN` counts as a read because it returns plan rows. `EXPLAIN ANALYZE` of a
/// DML statement does execute it, which is accepted for the same reason as above.
pub fn is_read_query(query: &str) -> bool {
    // Unscannable input: conservatively classify as a write. Callers should use
    // validate_query_syntax() first to get a proper error.
    let Ok(masked) = mask_sql_literals_and_comments(query) else {
        return false;
    };
    let upper = masked.to_uppercase();

    // Trailing semicolons (one or more, plus surrounding whitespace) are dropped
    // before looking for an embedded one.
    let stripped = upper.trim().trim_end_matches(';').trim();
    if stripped.is_empty() {
        return false;
    }
    if stripped.contains(';') {
        return false;
    }

    const READ_PREFIXES: &[&str] = &["SELECT", "SHOW", "PRAGMA", "DESCRIBE", "EXPLAIN"];

    if READ_PREFIXES
        .iter()
        .any(|p| stripped.starts_with(p) && is_keyword_at_boundary(stripped, p.len()))
    {
        return true;
    }

    if stripped.starts_with("WITH") && is_keyword_at_boundary(stripped, 4) {
        const WRITE_KEYWORDS: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE"];
        // Split on identifier boundaries rather than whitespace: a DML keyword
        // butted against a parenthesis, as in `(SELECT 1)INSERT` or the
        // data-modifying `WITH x AS (INSERT ...)`, is still a standalone token.
        return !stripped
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|word| WRITE_KEYWORDS.contains(&word));
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_read_query() {
        assert!(is_read_query("SELECT * FROM users"));
        assert!(is_read_query("  select * from users  "));
        assert!(is_read_query("SHOW TABLES"));
        assert!(is_read_query("PRAGMA table_info('users')"));
        assert!(is_read_query("EXPLAIN SELECT 1"));
        assert!(is_read_query("DESCRIBE users"));
        assert!(is_read_query("WITH cte AS (SELECT 1) SELECT * FROM cte"));

        assert!(!is_read_query("INSERT INTO users VALUES (1)"));
        assert!(!is_read_query("UPDATE users SET name = 'x'"));
        assert!(!is_read_query("DELETE FROM users"));
    }

    #[test]
    fn test_is_read_query_trailing_semicolon() {
        assert!(is_read_query("SELECT * FROM users;"));
        assert!(is_read_query("SELECT * FROM users ;"));
    }

    #[test]
    fn test_is_read_query_multi_statement() {
        assert!(!is_read_query("SELECT 1; DROP TABLE users"));
        assert!(!is_read_query("SELECT 1; SELECT 2"));
    }

    #[test]
    fn test_is_read_query_empty_and_whitespace() {
        assert!(!is_read_query(""));
        assert!(!is_read_query("   "));
        assert!(!is_read_query("\t\n"));
    }

    #[test]
    fn test_is_read_query_ddl() {
        assert!(!is_read_query("CREATE TABLE t (id INT)"));
        assert!(!is_read_query("ALTER TABLE t ADD COLUMN x INT"));
        assert!(!is_read_query("DROP TABLE t"));
    }

    #[test]
    fn test_is_read_query_cte_with_dml() {
        assert!(!is_read_query("WITH cte AS (SELECT 1) INSERT INTO users SELECT * FROM cte"));
        assert!(!is_read_query("WITH cte AS (SELECT 1) UPDATE users SET id = 1"));
        assert!(!is_read_query("WITH cte AS (SELECT 1) DELETE FROM users"));
    }

    #[test]
    fn test_is_read_query_explain_analyze() {
        assert!(is_read_query("EXPLAIN ANALYZE SELECT 1"));
        assert!(is_read_query("EXPLAIN ANALYZE DELETE FROM users"));
    }

    #[test]
    fn test_is_read_query_multiple_trailing_semicolons() {
        assert!(is_read_query("SELECT 1;;;"));
        // Spaces between semicolons are treated as embedded semicolons (multi-statement).
        assert!(!is_read_query("SELECT 1;  ;"));
    }

    #[test]
    fn test_is_read_query_sql_comments() {
        assert!(is_read_query("-- comment\nSELECT 1"));
        assert!(is_read_query("/* block comment */ SELECT 1"));
        assert!(is_read_query("-- line1\n-- line2\nSELECT 1"));
        assert!(is_read_query("/* comment */ -- another\nSELECT 1"));
        assert!(!is_read_query("-- comment\nINSERT INTO t VALUES (1)"));
    }

    #[test]
    fn test_is_read_query_semicolon_inside_string_literal() {
        // A quoted semicolon is data, not a statement separator: the read must
        // still be reported as a result set rather than an affected-row count.
        assert!(is_read_query("SELECT * FROM t WHERE s = 'a;b'"));
    }

    #[test]
    fn test_is_read_query_trailing_comment_after_semicolon() {
        assert!(is_read_query("SELECT 1; -- note"));
        assert!(is_read_query("SELECT 1; /* note */"));
    }

    #[test]
    fn test_is_read_query_trailing_semicolon_before_whitespace() {
        assert!(is_read_query("SELECT 1;\n"));
        assert!(is_read_query("SELECT 1; \t "));
    }

    #[test]
    fn test_is_read_query_cte_insert_without_whitespace() {
        // `1)INSERT` is one whitespace-delimited token but two SQL tokens.
        assert!(!is_read_query("WITH t AS (SELECT 1)INSERT INTO u VALUES (1)"));
    }

    #[test]
    fn test_is_read_query_cte_data_modifying_returning() {
        // PostgreSQL runs the INSERT even though the statement ends in SELECT.
        assert!(!is_read_query("WITH x AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM x"));
    }

    #[test]
    fn test_is_read_query_write_keyword_inside_literal() {
        // The keyword is quoted data, so the CTE is still read-only.
        assert!(is_read_query("WITH c AS (SELECT 1) SELECT 'INSERT' FROM c"));
    }

    #[test]
    fn test_is_read_query_unterminated_literal_is_not_read() {
        assert!(!is_read_query("SELECT 'unterminated FROM t"));
    }

    #[test]
    fn test_is_read_query_word_boundary() {
        assert!(!is_read_query("SELECTFOO"));
        assert!(is_read_query("SELECT(1)"));
        assert!(!is_read_query("SHOWING"));
        assert!(is_read_query("SHOW TABLES"));
    }
    #[test]
    fn test_mask_leaves_plain_sql_untouched() {
        assert_eq!(mask_sql_literals_and_comments("SELECT 1"), Ok("SELECT 1".to_string()));
    }

    #[test]
    fn test_mask_removes_line_comment_but_keeps_the_newline() {
        assert_eq!(
            mask_sql_literals_and_comments("-- comment\nSELECT 1"),
            Ok(" \nSELECT 1".to_string())
        );
    }

    #[test]
    fn test_mask_removes_block_comment() {
        assert_eq!(
            mask_sql_literals_and_comments("/* comment */ SELECT 1"),
            Ok("  SELECT 1".to_string())
        );
    }

    #[test]
    fn test_mask_line_comment_runs_to_end_of_input() {
        // Per SQL standard, `--` extends to end of input when there is no newline.
        assert_eq!(mask_sql_literals_and_comments("-- no newline"), Ok(" ".to_string()));
    }

    #[test]
    fn test_mask_removes_string_literal_contents() {
        assert_eq!(
            mask_sql_literals_and_comments("SELECT 'a;b' FROM t"),
            Ok("SELECT   FROM t".to_string())
        );
    }

    #[test]
    fn test_mask_treats_doubled_quote_as_an_escape() {
        // 'it''s' is one literal, not two, so nothing after it leaks out.
        assert_eq!(
            mask_sql_literals_and_comments("SELECT 'it''s' FROM t"),
            Ok("SELECT   FROM t".to_string())
        );
    }

    #[test]
    fn test_mask_removes_quoted_identifiers() {
        assert_eq!(
            mask_sql_literals_and_comments(r#"SELECT * FROM "my;table""#),
            Ok("SELECT * FROM  ".to_string())
        );
        assert_eq!(
            mask_sql_literals_and_comments("SELECT * FROM `my;table`"),
            Ok("SELECT * FROM  ".to_string())
        );
    }

    #[test]
    fn test_mask_preserves_multibyte_characters() {
        assert_eq!(
            mask_sql_literals_and_comments("SELECT '日本語' AS 列"),
            Ok("SELECT   AS 列".to_string())
        );
    }

    #[test]
    fn test_mask_rejects_unterminated_block_comment() {
        assert!(mask_sql_literals_and_comments("/* unterminated").is_err());
    }

    #[test]
    fn test_mask_rejects_slash_star_slash_as_unterminated() {
        // "/*/" is an unterminated block comment, NOT a zero-length comment.
        // The `*/` at positions 1-2 overlaps with the opening `/*` at positions 0-1,
        // so it must not be treated as a closing delimiter.
        assert!(mask_sql_literals_and_comments("/*/ SELECT 1").is_err());
    }

    #[test]
    fn test_mask_accepts_empty_block_comment() {
        assert_eq!(mask_sql_literals_and_comments("/**/SELECT 1"), Ok(" SELECT 1".to_string()));
    }

    #[test]
    fn test_validate_query_syntax() {
        assert!(validate_query_syntax("SELECT 1").is_ok());
        assert!(validate_query_syntax("-- comment\nSELECT 1").is_ok());
        assert!(validate_query_syntax("-- no newline").is_ok());
        assert!(validate_query_syntax("SELECT 'quoted' FROM t").is_ok());
        assert!(validate_query_syntax("/* unterminated").is_err());
    }

    #[test]
    fn test_validate_query_syntax_unterminated_string() {
        assert!(validate_query_syntax("SELECT 'unterminated FROM t").is_err());
    }

    #[test]
    fn test_is_keyword_at_boundary() {
        assert!(is_keyword_at_boundary("SELECT 1", 6));
        assert!(is_keyword_at_boundary("SELECT(1)", 6));
        assert!(is_keyword_at_boundary("SELECT", 6));
        assert!(!is_keyword_at_boundary("SELECTFOO", 6));
    }
}
