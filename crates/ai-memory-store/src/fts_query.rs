//! FTS5 `MATCH` query preparation for user/agent-supplied search text.
//!
//! FTS5 treats `column:term` as a column-qualified search. Natural-language
//! queries that contain bare colons (`pick: handoff`, `memory: bootstrap`) make
//! SQLite error with `no such column: pick` because only `title` and `body`
//! exist on the FTS tables. Unknown bare column syntax is neutralised without
//! discarding deliberate FTS operators such as `OR`.

/// Sanitize free-text for use in `WHERE pages_fts MATCH ?`.
///
/// Returns an empty string when `raw` is empty/whitespace-only; callers
/// should skip the SQL query in that case.
///
/// Bare multi-word queries are joined with **`OR`**, not the FTS5 default
/// (`AND`). A natural-language query like "cross project search strategy"
/// otherwise requires every word to co-occur in one page — near-zero recall
/// for anything but single keywords. With `OR` + bm25 ranking (callers
/// `ORDER BY rank`), the best-matching pages still surface first. When the
/// caller supplies explicit FTS5 syntax (`OR` / `AND` / `NOT` / `NEAR` /
/// quoted phrases / parens) we preserve it verbatim instead.
#[must_use]
pub fn prepare_fts5_query(raw: &str) -> String {
    let explicit_syntax = raw.contains('"')
        || raw.contains('(')
        || raw.contains(')')
        || raw
            .split_whitespace()
            .any(|t| matches!(t, "OR" | "AND" | "NOT" | "NEAR"));
    let tokens: Vec<String> = raw
        .split_whitespace()
        .flat_map(prepare_fts5_token)
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    let separator = if explicit_syntax { " " } else { " OR " };
    tokens.join(separator)
}

fn prepare_fts5_token(token: &str) -> Vec<String> {
    if has_unknown_bare_column(token) {
        return token
            .replace(':', " ")
            .split_whitespace()
            .map(quote_fts5_token)
            .collect();
    }

    if should_quote_fts5_token(token) {
        vec![quote_fts5_token(token)]
    } else {
        vec![token.to_string()]
    }
}

fn has_unknown_bare_column(token: &str) -> bool {
    token.contains(':')
        && !token.contains('"')
        && !token.starts_with("title:")
        && !token.starts_with("body:")
}

fn should_quote_fts5_token(token: &str) -> bool {
    token.contains('-') && !(token.starts_with('"') && token.ends_with('"'))
}

fn quote_fts5_token(token: &str) -> String {
    // FTS5 escapes `"` inside a quoted token by doubling it.
    let escaped = token.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colon_is_not_column_syntax() {
        // Bare multi-word → OR-joined (no explicit operator present).
        let q = prepare_fts5_query("pick: handoff ai-memory");
        assert_eq!(q, "\"pick\" OR handoff OR \"ai-memory\"");
    }

    #[test]
    fn bare_multi_word_is_or_joined() {
        // The recall fix: every word no longer has to co-occur.
        assert_eq!(
            prepare_fts5_query("cross project search strategy"),
            "cross OR project OR search OR strategy"
        );
    }

    #[test]
    fn portuguese_accented_terms_or_join_and_keep_accents() {
        // PT natural-language query: tokens preserved (accents intact),
        // joined with OR so a page matching any term is found.
        assert_eq!(
            prepare_fts5_query("descrição testes commits"),
            "descrição OR testes OR commits"
        );
    }

    #[test]
    fn single_word_has_no_or() {
        assert_eq!(prepare_fts5_query("handoff"), "handoff");
    }

    #[test]
    fn empty_yields_empty() {
        assert_eq!(prepare_fts5_query("   "), "");
    }

    #[test]
    fn quotes_are_escaped() {
        let q = quote_fts5_token(r#"say "hello""#);
        assert_eq!(q, r#""say ""hello""""#);
    }

    #[test]
    fn boolean_operators_are_preserved() {
        assert_eq!(prepare_fts5_query("quick OR slow"), "quick OR slow");
    }

    #[test]
    fn known_columns_are_preserved() {
        assert_eq!(prepare_fts5_query("title:handoff"), "title:handoff");
    }
}
