//! The network half of metadata lookup: one HTTPS GET to CrossRef or arXiv, with a timeout and a
//! size cap, over rustls with the operating system's trusted roots. Only ever started by the
//! user (a Look up button or the "look up new papers" preference); nothing else leaves the
//! machine — the request carries the DOI or arXiv id and nothing about the user.

use std::time::Duration;

use crate::model::Metadata;
use crate::{Error, Result, lookup};

/// The largest answer read.
const MAX_BODY: u64 = 4 << 20;

/// Metadata for a DOI from CrossRef.
pub fn crossref(doi: &str) -> Result<Metadata> {
    let body = get(&lookup::crossref_url(doi), "application/json")?;
    lookup::parse_crossref(&body)
}

/// Metadata for an arXiv id.
pub fn arxiv(id: &str) -> Result<Metadata> {
    let body = get(&lookup::arxiv_url(id), "application/atom+xml")?;
    lookup::parse_arxiv(&body)
}

/// Look up whichever identifier `meta` has: the DOI first (journal versions carry the venue),
/// then the arXiv id, then a CrossRef search by title (accepted only on a close match).
pub fn lookup(meta: &Metadata) -> Result<Metadata> {
    if !meta.doi.trim().is_empty() {
        match crossref(&meta.doi) {
            Ok(found) => return Ok(found),
            Err(e) if meta.arxiv.trim().is_empty() => return Err(e),
            Err(_) => {}
        }
    }
    if !meta.arxiv.trim().is_empty() {
        return arxiv(&meta.arxiv);
    }
    if meta.title.split_whitespace().count() >= 3 {
        let author = meta.authors.first().map(|a| a.family.as_str()).unwrap_or("");
        let body = get(&lookup::crossref_search_url(&meta.title, author), "application/json")?;
        return lookup::parse_crossref_search(&body, &meta.title);
    }
    Err(Error::Lookup("this paper has no DOI, arXiv id or title to look up".into()))
}

fn get(url: &str, accept: &str) -> Result<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(os_roots()?).build())
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .header("Accept", accept)
        // CrossRef asks API users to identify the tool; no personal data is sent.
        .header("User-Agent", concat!("PdfCraft-Library/", env!("CARGO_PKG_VERSION"), " (https://github.com/storytold/pdfcraft)"))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| Error::Lookup(format!("couldn't reach the metadata service ({e})")))?;
    let status = response.status().as_u16();
    if status == 404 {
        return Err(Error::Lookup("no record found for this identifier".into()));
    }
    if !(200..300).contains(&status) {
        return Err(Error::Lookup(format!("the metadata service answered with status {status}")));
    }
    response.body_mut().with_config().limit(MAX_BODY).read_to_string().map_err(|e| Error::Lookup(format!("unreadable answer ({e})")))
}

fn os_roots() -> Result<ureq::tls::RootCerts> {
    let found = rustls_native_certs::load_native_certs();
    let certs: Vec<ureq::tls::Certificate<'static>> = found.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
    if certs.is_empty() {
        return Err(Error::Lookup("no trusted certificates found on this system".into()));
    }
    Ok(ureq::tls::RootCerts::new_with_certs(&certs))
}

/// Ask Claude: POST a Messages API request built by [`crate::ask::request`] and parse the answer.
/// Sends the paper's text (and the reader's notes, when included) to Anthropic with the user's
/// own API key; nothing else.
pub fn ask(api_key: &str, request: &serde_json::Value) -> Result<crate::ask::Turn> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(os_roots()?).build())
        .build()
        .new_agent();
    let body = serde_json::to_string(request).map_err(|e| Error::Lookup(e.to_string()))?;
    let mut response = agent
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key.trim())
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .config()
        .http_status_as_error(false)
        .build()
        .send(body.as_str())
        .map_err(|e| Error::Lookup(format!("couldn't reach Claude ({e})")))?;
    let status = response.status().as_u16();
    let text = response.body_mut().with_config().limit(MAX_BODY).read_to_string().map_err(|e| Error::Lookup(format!("unreadable answer ({e})")))?;
    if !(200..300).contains(&status) {
        return Err(Error::Lookup(crate::ask::error_message(status, &text)));
    }
    crate::ask::parse_answer(&text)
}
