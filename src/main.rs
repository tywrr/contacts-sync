//! contacts-sync: makes contacts edited in vCard 4 / JMAP clients show up properly in Apple
//! Contacts, over CardDAV.
//!
//! JMAP and vCard 4 clients (for example the Bulwark webmail on a Stalwart server) and Apple
//! Contacts (vCard 3.0) store three things differently, and the server does not translate them,
//! so this tool does, on a schedule. The vCard 4 side is the source of truth; matching edits made
//! on an Apple device are overwritten.
//!
//! 1. Lists: each CATEGORIES value becomes an Apple group card with exactly its contacts.
//! 2. Relationships: `@spouse Jane Doe` lines in Notes become Apple related names and vCard
//!    RELATED (Stalwart exposes that to JMAP clients as JSContact relatedTo).
//! 3. Social profiles: SOCIALPROFILE (vCard 4, ignored by Apple) gets an X-SOCIALPROFILE mirror.
//!
//! Ported from an earlier Python script, keeping its flags and output.

mod dav;
mod sync;
mod vcard;

use anyhow::{Result, bail};
use clap::Parser;
use std::collections::BTreeMap;
use std::process::ExitCode;
use sync::{AppleList, Wanted};
use vcard::{fold, split_categories, unesc, values};

#[derive(Parser)]
#[command(
    version,
    about = "Make vCard 4 / JMAP contact data visible in Apple Contacts, over CardDAV"
)]
struct Args {
    /// Write the changes (without it, only print the plan).
    #[arg(long)]
    apply: bool,
    /// One-time: copy Apple relationships into Notes as @ lines (cards without @ lines).
    #[arg(long)]
    migrate_relationships: bool,
    /// CardDAV server base URL, e.g. https://dav.example.com (STALWART_URL is also read).
    #[arg(long, visible_alias = "base", env = "CARDDAV_URL")]
    url: Option<String>,
    /// Address book path on the server; {user} and {book} are filled in. Default: Stalwart's layout.
    #[arg(long, default_value = "/dav/card/{user}/{book}/")]
    book_path: String,
    /// Address book name under the account.
    #[arg(long, default_value = "default")]
    book: String,
    /// Most lists deleted in one run; above that nothing is written.
    #[arg(long, default_value_t = 3)]
    max_deletes: usize,
    /// Most contact cards rewritten in one run; above that nothing is written.
    #[arg(long, default_value_t = 40)]
    max_writes: usize,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Percent-encodes a path segment the way Python's `urllib.parse.quote` does (keeps `-_.~`).
fn quote(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn body(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| fold(l))
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

fn run(a: Args) -> Result<ExitCode> {
    let (Ok(user), Ok(password)) = (std::env::var("DAV_USER"), std::env::var("DAV_PASSWORD"))
    else {
        bail!("DAV_USER / DAV_PASSWORD not set");
    };
    let Some(base) = a.url.or_else(|| std::env::var("STALWART_URL").ok()) else {
        bail!("no server URL: pass --url or set CARDDAV_URL");
    };
    let dav = dav::Dav::new(&base, &user, &password)?;
    let book_path = a
        .book_path
        .replace("{user}", &quote(&user))
        .replace("{book}", &a.book);
    let cards = dav.report(&book_path)?;

    // ---- split cards into Apple lists and contacts; collect categories
    let mut wanted: Wanted = BTreeMap::new();
    let mut lists: BTreeMap<String, AppleList> = BTreeMap::new();
    let mut contacts = Vec::new();
    for card in &cards {
        let uid = unesc(values(&card.lines, "UID").first().copied().unwrap_or(""))
            .trim()
            .to_string();
        let apple_kind = values(&card.lines, "X-ADDRESSBOOKSERVER-KIND");
        let kind = apple_kind
            .first()
            .or(values(&card.lines, "KIND").first())
            .copied()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if kind == "group" {
            if !apple_kind.is_empty() {
                let name = unesc(values(&card.lines, "FN").first().copied().unwrap_or(""))
                    .trim()
                    .to_string();
                let members = values(&card.lines, "X-ADDRESSBOOKSERVER-MEMBER")
                    .iter()
                    .map(|m| m.trim().trim_start_matches("urn:uuid:").to_string())
                    .collect();
                lists.insert(
                    name,
                    AppleList {
                        href: card.href.clone(),
                        etag: card.etag.clone(),
                        uid,
                        members,
                    },
                );
            }
            continue;
        }
        if uid.is_empty() {
            continue;
        }
        for value in values(&card.lines, "CATEGORIES") {
            for cat in split_categories(value) {
                wanted.entry(cat).or_default().insert(uid.clone());
            }
        }
        contacts.push(card);
    }

    // ---- plan lists
    struct ListWrite<'a> {
        action: &'static str,
        name: &'a str,
        uid: String,
        href: String,
        etag: Option<String>,
    }
    let mut list_plan = Vec::new();
    for (name, members) in &wanted {
        match lists.get(name) {
            None => {
                let uid = uuid::Uuid::new_v5(&sync::LIST_NS, format!("list:{name}").as_bytes())
                    .to_string();
                let href = format!("{book_path}{uid}.vcf");
                list_plan.push(ListWrite {
                    action: "create",
                    name,
                    uid,
                    href,
                    etag: None,
                });
            }
            Some(cur) if &cur.members != members => list_plan.push(ListWrite {
                action: "update",
                name,
                uid: cur.uid.clone(),
                href: cur.href.clone(),
                etag: cur.etag.clone(),
            }),
            Some(_) => {}
        }
    }
    // Never delete when no categories were read at all (a bad read must not wipe every list).
    let deletes: Vec<(&String, &AppleList)> = if wanted.is_empty() {
        Vec::new()
    } else {
        lists
            .iter()
            .filter(|(n, _)| !wanted.contains_key(*n))
            .collect()
    };

    // ---- plan contact cards
    let mut card_plan = Vec::new();
    for card in &contacts {
        let change = sync::plan_card(&card.lines, a.migrate_relationships);
        if !change.why.is_empty() {
            let name = unesc(values(&card.lines, "FN").first().copied().unwrap_or("?"));
            card_plan.push((*card, name, sync::rewrite(&card.lines, &change), change.why));
        }
    }

    // ---- report
    println!(
        "{} cards: {} contacts, {} Apple lists, {} categories",
        cards.len(),
        contacts.len(),
        lists.len(),
        wanted.len()
    );
    for w in &list_plan {
        println!(
            "  list {:6} {:24} {:4} members",
            w.action,
            w.name,
            wanted[w.name].len()
        );
    }
    for (name, _) in &deletes {
        println!("  list delete {name:24} (no contact has this category any more)");
    }
    for (_, name, _, why) in &card_plan {
        println!("  card {name:30} {}", why.join("; "));
    }
    let mut blocked = Vec::new();
    if deletes.len() > a.max_deletes {
        blocked.push(format!(
            "{} list deletions > --max-deletes {}",
            deletes.len(),
            a.max_deletes
        ));
    }
    if card_plan.len() > a.max_writes {
        blocked.push(format!(
            "{} card rewrites > --max-writes {}",
            card_plan.len(),
            a.max_writes
        ));
    }
    for b in &blocked {
        println!("  !! {b}: writing nothing");
    }
    if list_plan.is_empty() && deletes.is_empty() && card_plan.is_empty() {
        println!("  nothing to change");
    }
    if !a.apply || !blocked.is_empty() {
        println!(
            "{}",
            if blocked.is_empty() {
                "dry run: nothing written"
            } else {
                "blocked: nothing written"
            }
        );
        return Ok(if blocked.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    // ---- write
    let mut failures = 0;
    let mut report = |label: &str, r: dav::WriteResult| {
        if r.ok() {
            println!("  {label}: HTTP {}", r.status);
        } else {
            failures += 1;
            println!(
                "  FAILED {label}: HTTP {} {}",
                r.status,
                r.error_body.chars().take(200).collect::<String>()
            );
        }
    };
    for w in &list_plan {
        let lines = sync::list_card(&w.uid, w.name, &wanted[w.name]);
        report(
            &format!("list {} {}", w.action, w.name),
            dav.put(&w.href, &body(&lines), w.etag.as_deref())?,
        );
    }
    for (name, list) in &deletes {
        report(
            &format!("list delete {name}"),
            dav.delete(&list.href, list.etag.as_deref())?,
        );
    }
    for (card, name, lines, _) in &card_plan {
        report(
            &format!("card {name}"),
            dav.put(&card.href, &body(lines), card.etag.as_deref())?,
        );
    }
    Ok(if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_like_python() {
        assert_eq!(super::quote("user@example.com"), "user%40example.com");
    }
}
