//! Minimal CardDAV client: one REPORT to read the address book, PUT/DELETE to write.

use anyhow::{Context, Result};
use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::time::Duration;
use ureq::http::Request;

/// One card as served: where it lives, its ETag (for If-Match), and its unfolded lines.
pub struct Card {
    pub href: String,
    pub etag: Option<String>,
    pub lines: Vec<String>,
}

/// Result of a write: the HTTP status, plus the body when the server refused it.
pub struct WriteResult {
    pub status: u16,
    pub error_body: String,
}

impl WriteResult {
    pub fn ok(&self) -> bool {
        self.status < 400
    }
}

pub struct Dav {
    agent: ureq::Agent,
    base: url::Url,
    auth: String,
}

const REPORT_BODY: &str = r#"<?xml version="1.0"?><C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav"><D:prop><D:getetag/><C:address-data/></D:prop></C:addressbook-query>"#;

impl Dav {
    pub fn new(base: &str, user: &str, password: &str) -> Result<Self> {
        let config = ureq::Agent::config_builder()
            // 4xx/5xx come back as normal responses so we can report the status ourselves.
            .http_status_as_error(false)
            // REPORT is a WebDAV method; ureq refuses non-standard methods unless allowed.
            .allow_non_standard_methods(true)
            .timeout_global(Some(Duration::from_secs(120)))
            .build();
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        Ok(Dav {
            agent: config.into(),
            base: url::Url::parse(&format!("{}/", base.trim_end_matches('/')))
                .context("invalid --base URL")?,
            auth: format!("Basic {token}"),
        })
    }

    fn url(&self, href: &str) -> Result<String> {
        Ok(self
            .base
            .join(href)
            .with_context(|| format!("bad href {href}"))?
            .to_string())
    }

    /// Reads every card in the address book at `book_path` (e.g. `/dav/card/ty%40x/default/`).
    pub fn report(&self, book_path: &str) -> Result<Vec<Card>> {
        let req = Request::builder()
            .method("REPORT")
            .uri(self.url(book_path)?)
            .header("Authorization", &self.auth)
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(REPORT_BODY)?;
        let mut resp = self.agent.run(req).context("REPORT request failed")?;
        let status = resp.status().as_u16();
        let xml = resp
            .body_mut()
            .read_to_string()
            .context("reading REPORT body")?;
        anyhow::ensure!(
            status == 207,
            "REPORT returned HTTP {status}: {}",
            &xml[..xml.len().min(200)]
        );
        parse_multistatus(&xml)
    }

    /// PUT a card body. `etag: Some` updates only if unchanged; `None` creates only if absent.
    pub fn put(&self, href: &str, body: &str, etag: Option<&str>) -> Result<WriteResult> {
        let mut req = Request::builder()
            .method("PUT")
            .uri(self.url(href)?)
            .header("Authorization", &self.auth)
            .header("Content-Type", "text/vcard; charset=utf-8");
        req = match etag {
            Some(tag) => req.header("If-Match", tag),
            None => req.header("If-None-Match", "*"),
        };
        self.send(req.body(body.as_bytes().to_vec())?)
    }

    pub fn delete(&self, href: &str, etag: Option<&str>) -> Result<WriteResult> {
        let mut req = Request::builder()
            .method("DELETE")
            .uri(self.url(href)?)
            .header("Authorization", &self.auth);
        if let Some(tag) = etag {
            req = req.header("If-Match", tag);
        }
        self.send(req.body(())?)
    }

    fn send(&self, req: Request<impl ureq::AsSendBody>) -> Result<WriteResult> {
        let mut resp = self.agent.run(req)?;
        let status = resp.status().as_u16();
        let error_body = if status >= 400 {
            resp.body_mut().read_to_string().unwrap_or_default()
        } else {
            String::new()
        };
        Ok(WriteResult { status, error_body })
    }
}

/// Which element's text we are currently collecting.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    None,
    Href,
    Etag,
    Data,
}

/// Parses a WebDAV multistatus: one `Card` per `<response>` that has an href and address-data.
pub fn parse_multistatus(xml: &str) -> Result<Vec<Card>> {
    let mut reader = Reader::from_str(xml);
    let mut cards = Vec::new();
    let (mut href, mut etag, mut data) = (String::new(), String::new(), None::<String>);
    let mut field = Field::None;

    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.local_name().as_ref() {
                "response" => (href, etag, data) = (String::new(), String::new(), None),
                "href" => field = Field::Href,
                "getetag" => field = Field::Etag,
                "address-data" => {
                    field = Field::Data;
                    data = Some(String::new());
                }
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "href" | "getetag" | "address-data" => field = Field::None,
                "response" => {
                    if let (false, Some(body)) = (href.trim().is_empty(), data.take()) {
                        let unfolded = crate::vcard::unfold(body.trim());
                        cards.push(Card {
                            href: href.trim().to_string(),
                            etag: Some(etag.trim().to_string()).filter(|t| !t.is_empty()),
                            lines: unfolded
                                .lines()
                                .filter(|l| !l.trim().is_empty())
                                .map(str::to_string)
                                .collect(),
                        });
                    }
                }
                _ => {}
            },
            // Text, CDATA and entity references (&amp; &#10; …) all append to the current field.
            Event::Text(t) => push(field, &mut href, &mut etag, &mut data, &t.into_inner()),
            Event::CData(t) => push(field, &mut href, &mut etag, &mut data, &t.into_inner()),
            Event::GeneralRef(r) => {
                let text = match r.resolve_char_ref()? {
                    Some(ch) => ch.to_string(),
                    None => match r.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        other => anyhow::bail!("unknown XML entity &{other};"),
                    }
                    .to_string(),
                };
                push(field, &mut href, &mut etag, &mut data, &text);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(cards)
}

fn push(field: Field, href: &mut String, etag: &mut String, data: &mut Option<String>, text: &str) {
    match field {
        Field::Href => href.push_str(text),
        Field::Etag => etag.push_str(text),
        Field::Data => data.get_or_insert_with(String::new).push_str(text),
        Field::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cards_with_entities_and_cdata() {
        let xml = r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
<D:response><D:href>/dav/card/a/default/1.vcf</D:href><D:propstat><D:prop>
<D:getetag>"e1"</D:getetag><C:address-data>BEGIN:VCARD&#13;
FN:Tom &amp; Jerry&#13;
NOTE:long line&#13;
  continued&#13;
END:VCARD</C:address-data></D:prop></D:propstat></D:response>
<D:response><D:href>/dav/card/a/default/2.vcf</D:href><D:propstat><D:prop>
<C:address-data><![CDATA[BEGIN:VCARD
FN:<b>
END:VCARD]]></C:address-data></D:prop></D:propstat></D:response>
</D:multistatus>"#;
        let cards = parse_multistatus(xml).unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].etag.as_deref(), Some("\"e1\""));
        assert_eq!(
            cards[0].lines,
            vec![
                "BEGIN:VCARD",
                "FN:Tom & Jerry",
                "NOTE:long line continued",
                "END:VCARD"
            ]
        );
        assert_eq!(cards[1].etag, None);
        assert_eq!(cards[1].lines[1], "FN:<b>");
    }
}
