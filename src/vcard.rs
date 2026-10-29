//! Text-level vCard 3.0 helpers.
//!
//! The sync never builds a full vCard model. It treats a card as a list of unfolded
//! content lines, edits only the lines it owns, and copies every other line through
//! untouched, so fields Apple or other clients care about but this tool does not know survive.

use regex::Regex;
use std::sync::LazyLock;

/// One content line split into its parts: `item3.X-ABLabel;TYPE=x:value`.
#[derive(Debug, PartialEq)]
pub struct Prop<'a> {
    /// Apple's `itemN` group prefix, lowercased (`item3`), if any.
    pub group: Option<String>,
    /// Property name, uppercased (`X-ABLABEL`).
    pub name: String,
    /// Raw parameter text including the leading `;` (`;TYPE=x`), or "".
    pub params: &'a str,
    /// Raw (still escaped) value after the first `:`.
    pub value: &'a str,
}

static PROP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:([A-Za-z0-9-]+)\.)?([A-Za-z0-9-]+)((?:;[^:]*)?):(.*)$").unwrap()
});

/// Parses one unfolded content line. Lines that are not `NAME[;params]:value` give `None`.
pub fn prop(line: &str) -> Option<Prop<'_>> {
    let c = PROP_RE.captures(line)?;
    Some(Prop {
        group: c.get(1).map(|g| g.as_str().to_lowercase()),
        name: c[2].to_uppercase(),
        params: c.get(3).map_or("", |m| m.as_str()),
        value: c.get(4).map_or("", |m| m.as_str()),
    })
}

/// Raw values of every line whose property name is `name` (already uppercase).
pub fn values<'a>(lines: &'a [String], name: &str) -> Vec<&'a str> {
    lines
        .iter()
        .filter_map(|l| prop(l))
        .filter(|p| p.name == name)
        .map(|p| p.value)
        .collect()
}

/// Escapes text for a vCard value (backslash, comma, semicolon, newline).
pub fn esc(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(';', "\\;")
        .replace('\n', "\\n")
}

/// Reverses `esc`: `\n`/`\N` become a newline, `\x` becomes `x`.
pub fn unesc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&'\n') | None => out.push(c), // a lone trailing backslash stays
                Some(_) => {
                    let n = chars.next().unwrap();
                    out.push(if n == 'n' || n == 'N' { '\n' } else { n });
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Joins folded lines (a line break followed by a space or tab continues the line).
pub fn unfold(body: &str) -> String {
    body.replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "")
        .replace("\n\t", "")
}

/// Folds a line at 75 octets (74 after the leading space of continuation lines),
/// never splitting a UTF-8 character.
pub fn fold(line: &str) -> String {
    if line.len() <= 75 {
        return line.to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        let limit = if parts.is_empty() { 75 } else { 74 };
        if cur.len() + ch.len_utf8() > limit {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    parts.push(cur);
    parts.join("\r\n ")
}

/// Splits a CATEGORIES value on unescaped commas, unescaping as it goes.
pub fn split_categories(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(n) => cur.push(n),
                None => cur.push(c),
            },
            ',' => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Python's `str.capitalize()`: first character uppercase, the rest lowercase.
pub fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_grouped_line() {
        let p = prop("item3.X-ABLabel;TYPE=x:_$!<Spouse>!$_").unwrap();
        assert_eq!(p.group.as_deref(), Some("item3"));
        assert_eq!(p.name, "X-ABLABEL");
        assert_eq!(p.params, ";TYPE=x");
        assert_eq!(p.value, "_$!<Spouse>!$_");
    }

    #[test]
    fn value_may_contain_colons() {
        let p = prop("URL:https://example.com/a:b").unwrap();
        assert_eq!(
            (p.name.as_str(), p.value),
            ("URL", "https://example.com/a:b")
        );
    }

    #[test]
    fn escape_round_trip() {
        let s = "a, b; c\\d\nnext";
        assert_eq!(unesc(&esc(s)), s);
        assert_eq!(unesc(r"x\N y\"), "x\n y\\");
    }

    #[test]
    fn folds_on_char_boundaries() {
        let long = "NOTE:".to_string() + &"é".repeat(60);
        let folded = fold(&long);
        assert!(folded.split("\r\n").all(|l| l.len() <= 75));
        assert_eq!(unfold(&folded), long);
    }

    #[test]
    fn splits_categories() {
        assert_eq!(
            split_categories(r"Friends,Book Club,A\,B, ,"),
            vec!["Friends", "Book Club", "A,B"]
        );
    }

    #[test]
    fn capitalizes_like_python() {
        assert_eq!(capitalize("bEST"), "Best");
        assert_eq!(capitalize(""), "");
    }
}
