//! Core search logic: DuckDuckGo HTML search + page content extraction.
//!
//! This module is transport agnostic: it is shared by the HTTP server
//! ([`crate::http_server`]) and the MCP server ([`crate::mcp_server`]).

use std::io::Cursor;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::{
    Client,
    header::{ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, HeaderMap, HeaderName, USER_AGENT},
};
use scraper::{Html, Selector};
use serde::Serialize;
use thiserror::Error;

pub const DEFAULT_MAX_RESULTS: usize = 3;

/// Hard cap of extracted content returned to the caller (LLM context economy).
pub const MAX_CONTENT_CHARS: usize = 6000;

/// A single search hit.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub content: String,
}

#[derive(Error, Debug)]
pub enum SearchError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("DuckDuckGo blocked the request (anomaly page) and the vqd retry did not help")]
    Blocked,
}

/// Shared HTTP client — connection pools are reused across requests.
fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // Generous timeouts — DDG and some content sites can take 10-20s.
        Client::builder()
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(10)
            .build()
            .expect("valid client configuration")
    })
}

/// Run a DuckDuckGo search and optionally fetch the content of each hit.
pub async fn web_search(
    query: &str,
    max_results: usize,
    fetch_content: bool,
) -> Result<Vec<SearchResult>, SearchError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let max_results = max_results.clamp(1, 20);
    tracing::info!(
        "Search: query='{query}', max_results={max_results}, fetch_content={fetch_content}"
    );

    let headers = build_headers();
    let ddg_url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    // Step 1 — fetch the DDG HTML page, with a single retry on failure.
    let mut html_response: Option<String> = None;
    for attempt in 0..2 {
        match client().get(&ddg_url).headers(headers.clone()).send().await {
            Ok(res) => {
                html_response = Some(res.text().await?);
                if attempt > 0 {
                    tracing::info!("DDG retry succeeded on attempt {}", attempt + 1);
                }
                break;
            }
            Err(e) if attempt == 0 => {
                tracing::warn!("DDG first attempt failed: {e}, retrying...");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }

    let html_response = html_response.ok_or(SearchError::Blocked)?;
    tracing::info!("DDG response length={}", html_response.len());

    // Step 2 — handle DDG's bot-detection ("anomaly") page via the vqd token.
    if html_response.contains("anomaly") || html_response.contains("anomaly-modal") {
        tracing::info!("Anomaly page detected, retrying with vqd");
        match extract_vqd(&html_response) {
            Some(vqd) => {
                let retry = get_results_with_vqd(client(), query, &vqd, &headers).await?;
                if !retry.is_empty() {
                    return parse_results(&retry, max_results, fetch_content, &headers).await;
                }
                tracing::warn!("vqd retry returned empty HTML for '{query}'");
            }
            None => tracing::error!("Could not extract vqd for '{query}'"),
        }
        return Err(SearchError::Blocked);
    }

    parse_results(&html_response, max_results, fetch_content, &headers).await
}

/// Format results as Markdown suitable for an LLM prompt.
pub fn render_markdown(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("# Web search: {query}\n\nNo results found.");
    }
    let mut out = String::new();
    out.push_str(&format!(
        "# Web search: {query}\n\nFound {} result(s):\n",
        results.len()
    ));
    for (idx, r) in results.iter().enumerate() {
        out.push_str(&format!("\n---\n\n## {}. {}\n\n", idx + 1, r.title));
        out.push_str(&format!("**URL:** {}\n\n", r.url));
        if !r.snippet.trim().is_empty() {
            out.push_str(&format!("**Snippet:** {}\n\n", r.snippet.trim()));
        }
        out.push_str("**Content:**\n\n");
        out.push_str(&truncate_content(&r.content));
        out.push('\n');
    }
    out
}

fn truncate_content(content: &str) -> String {
    let content = content.trim();
    if content.is_empty() {
        return "(контент недоступен)".to_string();
    }
    if content.chars().count() <= MAX_CONTENT_CHARS {
        return content.to_string();
    }
    let cut: String = content.chars().take(MAX_CONTENT_CHARS).collect();
    format!("{cut}\n… (content truncated)")
}

fn resolve_ddg_url(raw_href: &str) -> String {
    if raw_href.contains("duckduckgo.com/l/?uddg=")
        && let Some(pos) = raw_href.find("uddg=")
    {
        let encoded = &raw_href[pos + 5..];
        let encoded = encoded.split(['&', '#']).next().unwrap_or(encoded);
        if let Ok(decoded) = urlencoding::decode(encoded) {
            let url = decoded.to_string();
            if url.starts_with("http://") || url.starts_with("https://") {
                return url;
            } else if url.starts_with("//") || url.starts_with('/') {
                return format!("https:{}", url.trim_start_matches('/'));
            }
            return format!("https://{}", url);
        }
    }
    if raw_href.starts_with("//") {
        return format!("https:{}", raw_href);
    }
    raw_href.to_string()
}

fn strip_tags(html: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"<[^>]*>").expect("valid regex"));
    re.replace_all(html, "").trim().to_string()
}

fn build_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    fn insert(headers: &mut HeaderMap, name: HeaderName, value: &str) {
        headers.insert(name, value.parse().expect("valid header value"));
    }
    insert(
        &mut headers,
        USER_AGENT,
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    );
    insert(
        &mut headers,
        ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
    );
    insert(
        &mut headers,
        ACCEPT_LANGUAGE,
        "en-US,en;q=0.9,ru;q=0.8,ru-RU;q=0.7",
    );
    insert(&mut headers, ACCEPT_ENCODING, "gzip, deflate, br");
    insert(
        &mut headers,
        HeaderName::from_static("sec-ch-ua"),
        "\"Google Chrome\";v=\"131\", \"Chromium\";v=\"131\", \"Not/A:Brand\";v=\"99999999999\"",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-ch-ua-mobile"),
        "?0",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-ch-ua-platform"),
        "\"Linux\"",
    );
    // Use a realistic Linux kernel version (e.g. 6.x on modern distros)
    insert(
        &mut headers,
        HeaderName::from_static("sec-ch-ua-platform-version"),
        "\"6.8.0-31-generic\"",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-ch-ua-arch"),
        "\"x86\"",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-ch-ua-model"),
        "\"\"",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-ch-ua-full-version-list"),
        "\"Google Chrome\";v=\"131.0.0.0\", \"Chromium\";v=\"131.0.0.0\", \"Not/A:Brand\";v=\"99.0.0.0\"",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-fetch-dest"),
        "document",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-fetch-mode"),
        "navigate",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-fetch-site"),
        "none",
    );
    insert(
        &mut headers,
        HeaderName::from_static("sec-fetch-user"),
        "?1",
    );
    insert(
        &mut headers,
        HeaderName::from_static("upgrade-insecure-requests"),
        "1",
    );
    // Referer — many sites (including Wikipedia) check that the request
    // comes from a search engine rather than a bare client.
    insert(
        &mut headers,
        HeaderName::from_static("referer"),
        "https://duckduckgo.com/",
    );
    // Cookie stub — bypasses some bot-protection checks that look for
    // a real session (e.g. AWS WAF, Cloudflare).
    insert(
        &mut headers,
        HeaderName::from_static("cookie"),
        "sess=abc123; t=US; dc=US; iurl=1",
    );
    headers
}

fn extract_vqd(html: &str) -> Option<String> {
    // Pattern 1: q=...&o=...&vqd=...
    let vqd_pattern = r#"q=[^&]*&o=[^&]*&vqd=([^"&]*)"#;
    if let Some(cap) = regex::Regex::new(vqd_pattern).ok()?.captures(html)
        && let Ok(decoded) = urlencoding::decode(&cap[1])
    {
        return Some(decoded.to_string());
    }
    // Pattern 2: "vqd":"..."
    if let Some(pos) = html.find("\"vqd\":\"") {
        let start = pos + 9;
        if let Some(end) = html[start..].find("\"") {
            return Some(html[start..start + end].to_string());
        }
    }
    // Pattern 3: o=...&vqd=... (simplified from the form action)
    let vqd_simple = r#"o=[^&]*&vqd=([^"&]*)"#;
    if let Some(cap) = regex::Regex::new(vqd_simple).ok()?.captures(html)
        && let Ok(decoded) = urlencoding::decode(&cap[1])
    {
        return Some(decoded.to_string());
    }
    None
}

async fn get_results_with_vqd(
    client: &Client,
    query: &str,
    vqd: &str,
    headers: &HeaderMap,
) -> Result<String, SearchError> {
    let ddg_url = format!(
        "https://html.duckduckgo.com/html/?q={}&vqd={}",
        urlencoding::encode(query),
        vqd
    );
    Ok(client
        .get(&ddg_url)
        .headers(headers.clone())
        .send()
        .await?
        .text()
        .await?)
}

const CONTENT_SELECTORS: [&str; 20] = [
    // Most specific — usually the actual article body
    "article",
    "div.post-content",
    "div.article-body",
    "div.entry-content",
    "div.article__body",
    "div.text.content",
    "div.post-text",
    "div.story-body",
    "div.article_content",
    "div.article-content",
    // Coding / educational sites (coddy.tech, codeutility, etc.)
    "div.main-content",
    "div.main",
    "section.article-content",
    "div#main",
    "div#content-area",
    // Broader — may include sidebar/footer, used as fallback
    "div#content",
    "div#main-content",
    "main",
    "div.content",
    "div.body",
];

fn extract_main_content(html: &str) -> String {
    let document = Html::parse_document(html);

    for selector_str in &CONTENT_SELECTORS {
        if let Ok(sel) = Selector::parse(selector_str) {
            let elements: Vec<_> = document.select(&sel).collect();
            if !elements.is_empty() {
                let inner = elements[0].inner_html();
                let text = html2text::from_read(Cursor::new(inner), 120);
                if !looks_like_noise(&text) {
                    return text;
                }
            }
        }
    }

    // Fallback: convert entire page to text
    let text = html2text::from_read(Cursor::new(html), 120);
    if looks_like_noise(&text) {
        String::new()
    } else {
        text
    }
}

fn looks_like_noise(text: &str) -> bool {
    let trimmed = text.trim();
    let char_count = trimmed.chars().count();
    // Very short = probably not useful
    if char_count < 50 {
        return true;
    }
    // Very long without paragraph breaks = probably a single block of noise
    let paragraph_count = trimmed.matches('\n').count();
    if char_count > 300 && paragraph_count < 3 {
        return true;
    }
    // Check for high ratio of URL-like content (links everywhere = navigation)
    let url_count = trimmed.matches("http").count()
        + trimmed.matches(".com").count()
        + trimmed.matches(".ru").count()
        + trimmed.matches("geeksforgeeks").count();
    if url_count > 5 && char_count < 1500 {
        return true;
    }
    // Repeated section headers like "Home | About | Contact"
    if trimmed.contains(" | ") && trimmed.matches(" | ").count() > 5 {
        return true;
    }
    // Starts with navigation menu items (common pattern on many sites)
    if trimmed.starts_with("[") && trimmed.lines().count() > 10 {
        let first_lines: Vec<_> = trimmed.lines().take(5).collect();
        let all_look_like_links = first_lines.iter().all(|line| {
            let l = line.trim();
            l.starts_with("[") || l.contains("https://") || l.len() < 30
        });
        if all_look_like_links && trimmed.matches("https://").count() > 3 {
            return true;
        }
    }
    false
}

async fn parse_results(
    html: &str,
    max_results: usize,
    do_fetch_content: bool,
    headers: &HeaderMap,
) -> Result<Vec<SearchResult>, SearchError> {
    let body_selector = Selector::parse("div.result__body").expect("valid selector");

    struct RawResult {
        title: String,
        url: String,
        snippet: String,
    }

    let raw: Vec<RawResult> = {
        let document = Html::parse_document(html);
        let count = document.select(&body_selector).count();
        tracing::info!("Found {count} result bodies");

        document
            .select(&body_selector)
            .filter_map(|body| {
                let title_el = body.select(&Selector::parse("a.result__a").ok()?).next()?;
                let raw_html = title_el.inner_html();
                let title = strip_tags(&raw_html);

                let raw_href = title_el.value().attr("href")?;
                let url = resolve_ddg_url(raw_href);

                let snippet = body
                    .select(&Selector::parse("a.result__snippet").ok()?)
                    .next()
                    .map(|s| strip_tags(&s.inner_html()))
                    .unwrap_or_default();

                // Filter out empty titles (usually navigation items, not results)
                if title.trim().is_empty() {
                    return None;
                }
                if url.is_empty() {
                    return None;
                }

                Some(RawResult {
                    title,
                    url,
                    snippet,
                })
            })
            .collect()
    };

    let mut results = Vec::new();
    for raw in raw {
        let content = if do_fetch_content {
            fetch_content(&raw.url, headers).await
        } else {
            String::new()
        };
        results.push(SearchResult {
            title: raw.title,
            url: raw.url,
            snippet: raw.snippet,
            content,
        });
    }

    results.truncate(max_results);
    Ok(results)
}

async fn fetch_content(url: &str, headers: &HeaderMap) -> String {
    // ── Phase 1: fetch with browser headers, capture Set-Cookie ──
    let mut cookie_header = String::new();

    let res = match client().get(url).headers(headers.clone()).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::info!("Fetch error for {url}: {e}");
            return String::new();
        }
    };

    // Extract Set-Cookie headers (sites like metanit.com require them)
    for val in res.headers().get_all("set-cookie").iter() {
        if !cookie_header.is_empty() {
            cookie_header.push_str("; ");
        }
        cookie_header.push_str(val.to_str().unwrap_or(""));
    }

    let status = res.status();
    let html = res.text().await.unwrap_or_default();

    // ── Phase 2: retry with cookies if 403 ──
    let (status, final_html) = if status == 403 && !cookie_header.is_empty() {
        let mut retry_headers = headers.clone();
        if let Ok(cookie_val) = cookie_header.parse() {
            retry_headers.insert(HeaderName::from_static("cookie"), cookie_val);
        }
        match client().get(url).headers(retry_headers).send().await {
            Ok(r) => {
                let s = r.status();
                let h = r.text().await.unwrap_or_default();
                (s, h)
            }
            Err(e) => {
                tracing::info!("Fetch retry error for {url}: {e}");
                (status, html)
            }
        }
    } else {
        (status, html)
    };

    if !status.is_success() {
        tracing::info!("Fetch {status} for {url}");
        return String::new();
    }
    if final_html.len() < 100 {
        // Likely a redirect page or very short content
        return String::new();
    }
    extract_main_content(&final_html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ddg_redirect_link() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&rut=abc";
        assert_eq!(resolve_ddg_url(href), "https://example.com/a");
        assert_eq!(
            resolve_ddg_url("https://example.com/b"),
            "https://example.com/b"
        );
    }

    #[test]
    fn markdown_marks_missing_content() {
        let results = vec![SearchResult {
            title: "T".into(),
            url: "https://example.com".into(),
            snippet: "S".into(),
            content: String::new(),
        }];
        let md = render_markdown("q", &results);
        assert!(md.contains("## 1. T"));
        assert!(md.contains("**URL:** https://example.com"));
        assert!(md.contains("(контент недоступен)"));
    }

    #[test]
    fn content_is_truncated() {
        let big = "x".repeat(MAX_CONTENT_CHARS + 10);
        assert!(truncate_content(&big).ends_with("(content truncated)"));
    }
}
