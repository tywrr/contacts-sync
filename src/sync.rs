//! Planning: compares what the vCard 4 side says (categories, Notes `@` lines, online services)
//! with what Apple can read, and works out which cards to write.

use crate::vcard::{capitalize, esc, prop, unesc, values};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

/// Namespace for list card UIDs. Fixed, so the same category always maps to the same card.
pub const LIST_NS: uuid::Uuid = uuid::uuid!("6f6e1b2a-3c4d-4e5f-8a9b-0c1d2e3f4a5b");
pub const PRODID: &str = "-//tywrr//contacts-sync//EN";

/// `@type` values that have an Apple built-in label (`_$!<Spouse>!$_`).
const APPLE_BUILTIN: &[&str] = &[
    "spouse",
    "partner",
    "father",
    "mother",
    "parent",
    "brother",
    "sister",
    "child",
    "friend",
    "assistant",
    "manager",
];

/// `@type` -> RFC 6350 RELATED type, which Stalwart maps into JSContact `relatedTo` for Bulwark.
fn related_type(t: &str) -> Option<&'static str> {
    Some(match t {
        "spouse" | "wife" | "husband" => "spouse",
        "partner" => "sweetheart",
        "father" | "mother" | "parent" => "parent",
        "son" | "daughter" | "child" => "child",
        "brother" | "sister" | "sibling" => "sibling",
        "friend" => "friend",
        "manager" | "assistant" | "coworker" => "co-worker",
        "colleague" => "colleague",
        "neighbor" => "neighbor",
        _ => return None,
    })
}

/// Social service from the URL host when the card stored none.
fn social_host(host: &str) -> Option<&'static str> {
    Some(match host {
        "linkedin.com" => "linkedin",
        "twitter.com" | "x.com" => "twitter",
        "facebook.com" => "facebook",
        "instagram.com" => "instagram",
        "github.com" => "github",
        "flickr.com" => "flickr",
        "youtube.com" => "youtube",
        "tiktok.com" => "tiktok",
        "mastodon.social" => "mastodon",
        "bsky.app" => "bluesky",
        "threads.net" => "threads",
        _ => return None,
    })
}

/// A relationship: (`@type`, person's name).
pub type Relation = (String, String);

pub fn label_for(rtype: &str) -> String {
    if APPLE_BUILTIN.contains(&rtype) {
        format!("_$!<{}>!$_", capitalize(rtype))
    } else {
        rtype
            .split('-')
            .map(capitalize)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

static BUILTIN_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^_\$!<(.+)>!\$_$").unwrap());
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static AT_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@([A-Za-z][\w-]*)\s+(.+?)\s*$").unwrap());
static SERVICE_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)SERVICE-TYPE=([^;:]+)").unwrap());
static TYPE_PARAM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)TYPE=([^;:]+)").unwrap());
static HOST_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(www|m|mobile)\.").unwrap());
static ITEM_NO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^item(\d+)\.").unwrap());

/// `_$!<Father>!$_` -> `father`, `Best Friend` -> `best-friend`, missing -> `related`.
pub fn type_from_label(label: Option<&str>) -> String {
    let text = match label.and_then(|l| BUILTIN_LABEL.captures(l)) {
        Some(c) => c[1].to_string(),
        None => label.unwrap_or("related").to_string(),
    };
    let t = WHITESPACE
        .replace_all(&text.trim().to_lowercase(), "-")
        .into_owned();
    if t.is_empty() { "related".into() } else { t }
}

pub fn social_service(params: &str, url: &str) -> String {
    if let Some(c) = SERVICE_TYPE.captures(params) {
        return c[1].trim_matches('"').to_lowercase();
    }
    let full = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let host = url::Url::parse(&full)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .unwrap_or_default();
    let host = HOST_PREFIX.replace(&host, "").into_owned();
    if let Some(s) = social_host(&host) {
        return s.into();
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() >= 2 {
        parts[parts.len() - 2].to_string()
    } else {
        "other".into()
    }
}

/// Apple relationships on a card (in line order) and the `itemN` groups they use.
pub fn apple_relations(lines: &[String]) -> (Vec<Relation>, HashSet<String>) {
    let labels: HashMap<String, String> = lines
        .iter()
        .filter_map(|l| prop(l))
        .filter(|p| p.name == "X-ABLABEL")
        .filter_map(|p| Some((p.group?, p.value.to_string())))
        .collect();
    let mut rel = Vec::new();
    let mut groups = HashSet::new();
    for p in lines
        .iter()
        .filter_map(|l| prop(l))
        .filter(|p| p.name == "X-ABRELATEDNAMES")
    {
        let label = p.group.as_ref().and_then(|g| labels.get(g));
        let rtype = if label.is_some() {
            type_from_label(label.map(String::as_str))
        } else {
            "related".into()
        };
        rel.push((rtype, unesc(p.value).trim().to_string()));
        if let Some(g) = p.group {
            groups.insert(g);
        }
    }
    (rel, groups)
}

/// `(type, name) -> original X-ABLabel`, so rewrites keep labels Apple localises.
fn original_labels(lines: &[String]) -> HashMap<Relation, String> {
    let labels: HashMap<String, String> = lines
        .iter()
        .filter_map(|l| prop(l))
        .filter(|p| p.name == "X-ABLABEL")
        .filter_map(|p| Some((p.group?, p.value.to_string())))
        .collect();
    lines
        .iter()
        .filter_map(|l| prop(l))
        .filter(|p| p.name == "X-ABRELATEDNAMES")
        .filter_map(|p| {
            let label = labels.get(p.group.as_ref()?)?;
            Some((
                (
                    type_from_label(Some(label)),
                    unesc(p.value).trim().to_string(),
                ),
                label.clone(),
            ))
        })
        .collect()
}

/// `@` lines in the card's first NOTE, and the whole note text. `None` when there is no NOTE.
pub fn note_relations(lines: &[String]) -> Option<(Vec<Relation>, String)> {
    let text = unesc(values(lines, "NOTE").first()?);
    let rel = text
        .lines()
        .filter_map(|l| AT_LINE.captures(l.trim_end_matches('\r')))
        .map(|c| (c[1].to_lowercase(), c[2].to_string()))
        .collect();
    Some((rel, text))
}

/// Social profiles as (service, url): vCard 4 SOCIALPROFILE lines, and Apple's mirrors.
fn socials(lines: &[String]) -> (BTreeSet<Relation>, BTreeSet<Relation>) {
    let mut want = BTreeSet::new();
    let mut have = BTreeSet::new();
    for p in lines.iter().filter_map(|l| prop(l)) {
        match p.name.as_str() {
            "SOCIALPROFILE" => {
                want.insert((social_service(p.params, p.value), p.value.to_string()));
            }
            "X-SOCIALPROFILE" => {
                let svc = TYPE_PARAM
                    .captures(p.params)
                    .map_or("other".into(), |c| c[1].to_lowercase());
                have.insert((svc, p.value.to_string()));
            }
            _ => {}
        }
    }
    (want, have)
}

/// What to change on one card. `None` fields are left as they are.
#[derive(Default)]
pub struct CardChange {
    pub relations: Option<Vec<Relation>>,
    pub socials: Option<Vec<Relation>>,
    pub new_note: Option<String>,
    pub why: Vec<String>,
}

/// Decides what (if anything) to change on a contact card.
pub fn plan_card(lines: &[String], migrate: bool) -> CardChange {
    let mut change = CardChange::default();
    let (apple_rel, _) = apple_relations(lines);
    let (mut note_rel, note_text) = match note_relations(lines) {
        Some((rel, text)) => (rel, Some(text)),
        None => (Vec::new(), None),
    };

    // One-time import: Apple's relationships become @ lines in Notes.
    if migrate && !apple_rel.is_empty() && note_rel.is_empty() {
        let add = apple_rel
            .iter()
            .map(|(t, n)| format!("@{t} {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        change.new_note = Some(
            match note_text.as_deref().filter(|t| !t.trim().is_empty()) {
                Some(text) => format!("{}\n\n{add}", text.trim_end()),
                None => add,
            },
        );
        note_rel = apple_rel.clone();
        change
            .why
            .push(format!("notes += {} @ line(s)", apple_rel.len()));
    }

    // Relationships: cards without @ lines are left alone.
    if !note_rel.is_empty() {
        let mut related: Vec<(String, String)> = lines
            .iter()
            .filter_map(|l| prop(l))
            .filter(|p| p.name == "RELATED")
            // Compare only the TYPE value: Stalwart adds parameters such as CHARSET=UTF-8
            // for non-ASCII names, which must not count as a difference.
            .map(|p| {
                let rtype = TYPE_PARAM
                    .captures(p.params)
                    .map_or(String::new(), |c| c[1].to_uppercase());
                (rtype, unesc(p.value))
            })
            .collect();
        let mut want_related: Vec<(String, String)> = note_rel
            .iter()
            .map(|(t, n)| (related_type(t).unwrap_or("").to_uppercase(), n.clone()))
            .collect();
        related.sort();
        want_related.sort();
        let (mut a, mut b) = (apple_rel.clone(), note_rel.clone());
        a.sort();
        b.sort();
        if a != b || related != want_related {
            let list = note_rel
                .iter()
                .map(|(t, n)| format!("{t} {n}"))
                .collect::<Vec<_>>()
                .join(", ");
            change.why.push(format!("relationships -> {list}"));
            change.relations = Some(note_rel);
        }
    }

    let (want, have) = socials(lines);
    if want != have {
        let list: Vec<String> = want.iter().map(|(s, u)| format!("{s} {u}")).collect();
        change.why.push(format!(
            "social -> {}",
            if list.is_empty() {
                "none".into()
            } else {
                list.join(", ")
            }
        ));
        change.socials = Some(want.into_iter().collect());
    }
    change
}

/// New card lines: relationship/social/NOTE lines replaced, everything else kept in place.
pub fn rewrite(lines: &[String], change: &CardChange) -> Vec<String> {
    let rel_groups = if change.relations.is_some() {
        apple_relations(lines).1
    } else {
        HashSet::new()
    };
    let keep_label = if change.relations.is_some() {
        original_labels(lines)
    } else {
        HashMap::new()
    };
    let mut note = change.new_note.clone();
    let mut out = Vec::new();
    for l in lines {
        let Some(p) = prop(l) else {
            out.push(l.clone());
            continue;
        };
        let owned_relation = change.relations.is_some()
            && ((p.group.as_ref().is_some_and(|g| rel_groups.contains(g))
                && (p.name == "X-ABRELATEDNAMES" || p.name == "X-ABLABEL"))
                || p.name == "RELATED"
                || (p.name == "JSPROP" && p.params.contains("JSPTR=\"relatedTo/")));
        if owned_relation || (change.socials.is_some() && p.name == "X-SOCIALPROFILE") {
            continue;
        }
        if p.name == "END" {
            break;
        }
        if p.name == "NOTE" && note.is_some() {
            out.push(format!("NOTE:{}", esc(&note.take().unwrap_or_default())));
            continue;
        }
        out.push(l.clone());
    }
    if let Some(text) = note {
        out.push(format!("NOTE:{}", esc(&text)));
    }
    if let Some(relations) = &change.relations {
        let first = out
            .iter()
            .filter_map(|l| ITEM_NO.captures(l))
            .filter_map(|c| c[1].parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        for (next, (rtype, name)) in (first..).zip(relations) {
            out.push(format!("item{next}.X-ABRELATEDNAMES:{}", esc(name)));
            if rtype != "related" {
                let label = keep_label
                    .get(&(rtype.clone(), name.clone()))
                    .cloned()
                    .unwrap_or_else(|| esc(&label_for(rtype)));
                out.push(format!("item{next}.X-ABLabel:{label}"));
            }
            out.push(match related_type(rtype) {
                Some(rt) => format!("RELATED;TYPE={rt}:{}", esc(name)),
                None => format!("RELATED:{}", esc(name)),
            });
        }
    }
    if let Some(socials) = &change.socials {
        out.extend(
            socials
                .iter()
                .map(|(svc, url)| format!("X-SOCIALPROFILE;TYPE={svc}:{url}")),
        );
    }
    out.push("END:VCARD".into());
    out
}

/// An Apple list card as currently served.
pub struct AppleList {
    pub href: String,
    pub etag: Option<String>,
    pub uid: String,
    pub members: BTreeSet<String>,
}

/// Group card body for an Apple list.
pub fn list_card(uid: &str, name: &str, members: &BTreeSet<String>) -> Vec<String> {
    let mut lines = vec![
        "BEGIN:VCARD".to_string(),
        "VERSION:3.0".into(),
        format!("PRODID:{PRODID}"),
        format!("UID:{uid}"),
        format!("N:{};;;;", esc(name)),
        format!("FN:{}", esc(name)),
        "X-ADDRESSBOOKSERVER-KIND:group".into(),
    ];
    lines.extend(
        members
            .iter()
            .map(|m| format!("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:{m}")),
    );
    lines.push("END:VCARD".into());
    lines
}

/// Categories on contacts: category -> UIDs carrying it (sorted, like the output).
pub type Wanted = BTreeMap<String, BTreeSet<String>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn card(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn labels_round_trip() {
        assert_eq!(label_for("spouse"), "_$!<Spouse>!$_");
        assert_eq!(label_for("best-friend"), "Best Friend");
        assert_eq!(type_from_label(Some("_$!<Father>!$_")), "father");
        assert_eq!(type_from_label(Some("Best  Friend")), "best-friend");
        assert_eq!(type_from_label(None), "related");
    }

    #[test]
    fn social_service_from_host() {
        assert_eq!(
            social_service("", "https://www.linkedin.com/in/x/"),
            "linkedin"
        );
        assert_eq!(
            social_service(";SERVICE-TYPE=\"LinkedIn\"", "x"),
            "linkedin"
        );
        assert_eq!(social_service("", "https://www.example.org/"), "example");
        assert_eq!(social_service("", "not a url"), "other");
    }

    #[test]
    fn migrates_apple_relations_into_notes() {
        let lines = card(&[
            "BEGIN:VCARD",
            "FN:John Doe",
            "NOTE:Met at a conference",
            "item1.X-ABRELATEDNAMES:Jane Doe",
            "item1.X-ABLabel:_$!<Spouse>!$_",
            "END:VCARD",
        ]);
        let change = plan_card(&lines, true);
        assert_eq!(
            change.new_note.as_deref(),
            Some("Met at a conference\n\n@spouse Jane Doe")
        );
        let out = rewrite(&lines, &change);
        assert!(out.contains(&"NOTE:Met at a conference\\n\\n@spouse Jane Doe".to_string()));
        // The old item1 lines are dropped first, so numbering restarts at item1.
        assert!(
            out.contains(&"item1.X-ABLabel:_$!<Spouse>!$_".to_string()),
            "{out:?}"
        );
        assert!(out.contains(&"RELATED;TYPE=spouse:Jane Doe".to_string()));
        assert_eq!(out.last().unwrap(), "END:VCARD");
    }

    #[test]
    fn synced_card_is_stable() {
        let lines = card(&[
            "BEGIN:VCARD",
            "NOTE:@godfather Alex Example",
            "item1.X-ABRELATEDNAMES:Alex Example",
            "item1.X-ABLABEL:Godfather",
            "RELATED:Alex Example",
            "END:VCARD",
        ]);
        let change = plan_card(&lines, false);
        assert!(change.why.is_empty(), "{:?}", change.why);
    }

    #[test]
    fn charset_parameter_does_not_force_a_rewrite() {
        let lines = card(&[
            "BEGIN:VCARD",
            "NOTE;CHARSET=UTF-8:@friend Zo\u{eb} Ex\u{e4}mple\\, Jr.",
            "item1.X-ABRELATEDNAMES;CHARSET=UTF-8:Zo\u{eb} Ex\u{e4}mple\\, Jr.",
            "item1.X-ABLabel:_$!<Friend>!$_",
            "RELATED;TYPE=FRIEND;CHARSET=UTF-8:Zo\u{eb} Ex\u{e4}mple, Jr.",
            "END:VCARD",
        ]);
        let why = plan_card(&lines, false).why;
        assert!(why.is_empty(), "{why:?}");
    }

    #[test]
    fn cards_without_at_lines_keep_apple_relations() {
        let lines = card(&["BEGIN:VCARD", "item1.X-ABRELATEDNAMES:Pat", "END:VCARD"]);
        assert!(plan_card(&lines, false).why.is_empty());
    }
}
