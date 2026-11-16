use axum::{Router, extract::Json, routing::post};
use reqwest::{
    Client,
    header::{ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, HeaderMap, HeaderName, USER_AGENT},
};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

fn resolve_ddg_url(raw_href: &str) -> String {
    if raw_href.contains("duckduckgo.com/l/?uddg=") {
        if let Some(pos) = raw_href.find("uddg=") {
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
    }
    if raw_href.starts_with("//") {
        return format!("https:{}", raw_href);
    }
    raw_href.to_string()
}

fn strip_tags(html: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"<[^>]*>").unwrap());
    re.replace_all(html, "").trim().to_string()
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default)]
    fetch_content: bool,
}

#[derive(Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    content: String,
}

fn default_max_results() -> usize {
    3
}

fn build_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
            .parse()
            .unwrap(),
    );
    headers.insert(
        ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
            .parse()
            .unwrap(),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        "en-US,en;q=0.9,ru;q=0.8,ru-RU;q=0.7".parse().unwrap(),
    );
    headers.insert(ACCEPT_ENCODING, "gzip, deflate, br".parse().unwrap());
    headers.insert(
        HeaderName::from_static("sec-ch-ua"),
        "\"Google Chrome\";v=\"131\", \"Chromium\";v=\"131\", \"Not/A:Brand\";v=\"99999999999\""
            .parse()
            .unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua-mobile"),
        "?0".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua-platform"),
        "\"Linux\"".parse().unwrap(),
    );
    // Use a realistic Linux kernel version (e.g. 6.x on modern distros)
    headers.insert(
        HeaderName::from_static("sec-ch-ua-platform-version"),
        "\"6.8.0-31-generic\"".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua-arch"),
        "\"x86\"".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua-model"),
        "\"\"".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua-full-version-list"),
        "\"Google Chrome\";v=\"131.0.0.0\", \"Chromium\";v=\"131.0.0.0\", \"Not/A:Brand\";v=\"99.0.0.0\""
            .parse()
            .unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-dest"),
        "document".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-mode"),
        "navigate".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-site"),
        "none".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-user"),
        "?1".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("upgrade-insecure-requests"),
        "1".parse().unwrap(),
    );
    // Referer — many sites (including Wikipedia) check that the request
    // comes from a search engine rather than a bare client.
    headers.insert(
        HeaderName::from_static("referer"),
        "https://duckduckgo.com/".parse().unwrap(),
    );
    // Cookie stub — bypasses some bot-protection checks that look for
    // a real session (e.g. AWS WAF, Cloudflare).
    headers.insert(
        HeaderName::from_static("cookie"),
        "sess=abc123; t=US; dc=US; iurl=1".parse().unwrap(),
    );
    headers
}

fn build_client() -> Client {
    // Generous timeouts — DDG and some content sites can take 10-20s.
    Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_max_idle_per_host(10)
        .build()
        .unwrap()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let app = Router::new().route("/search", post(search));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3333").await.unwrap();
    tracing::info!("Rust Search Server running on port 3333");
    axum::serve(listener, app).await.unwrap();
}

async fn search(Json(payload): Json<SearchRequest>) -> Json<Vec<SearchResult>> {
    let query = &payload.query;
    tracing::info!(
        "Search: query='{}', max_results={}, fetch_content={}",
        query,
        payload.max_results,
        payload.fetch_content
    );
    let client = build_client();
    let headers = build_headers();

    // Step 1 — fetch initial DDG page to get vqd (anti-bot token)
    let ddg_url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    // Try the initial DDG request, with a single retry on timeout.
    let mut html_response: Option<String> = None;
    for attempt in 0..2 {
        if let Ok(res) = client.get(&ddg_url).headers(headers.clone()).send().await {
            let text = res.text().await.unwrap_or_default();
            if attempt > 0 {
                tracing::info!("DDG retry succeeded on attempt {}", attempt + 1);
            }
            html_response = Some(text);
            break;
        } else if attempt == 0 {
            tracing::info!("DDG first attempt timed out, retrying...");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    }

    // If both attempts failed, return empty results
    let html_response = match html_response {
        Some(h) => h,
        None => {
            tracing::error!("DDG both attempts failed for '{}'", query);
            return Json(vec![]);
        }
    };

    // Parse status from the response for logging
    {
        // We already consumed the body, so log based on response presence
        let len = html_response.len();
        tracing::info!("DDG response length={}", len);
    }

    // Handle anomaly page (DDG's bot detection)
    if html_response.contains("anomaly") || html_response.contains("anomaly-modal") {
        tracing::info!("Anomaly page detected, retrying with vqd");
        let vqd = extract_vqd(&html_response);
        if let Some(vqd) = vqd {
            if let Ok(html) = get_results_with_vqd(&client, query, &vqd, &headers).await {
                if !html.is_empty() {
                    return parse_results(
                        &html,
                        payload.max_results,
                        &client,
                        payload.fetch_content,
                        &headers,
                    )
                    .await;
                }
                tracing::warn!("vqd retry returned empty HTML for '{}'", query);
            } else {
                tracing::error!("vqd retry failed for '{}'", query);
            }
        } else {
            tracing::error!("Could not extract vqd for '{}'", query);
        }
        // Fall through — will return empty results
    }

    parse_results(
        &html_response,
        payload.max_results,
        &client,
        payload.fetch_content,
        &headers,
    )
    .await
}

fn extract_vqd(html: &str) -> Option<String> {
    // Pattern 1: q=...&o=...&vqd=...
    let vqd_pattern = r#"q=[^&]*&o=[^&]*&vqd=([^"&]*)"#;
    if let Some(cap) = regex::Regex::new(vqd_pattern).ok()?.captures(html) {
        if let Ok(decoded) = urlencoding::decode(&cap[1]) {
            return Some(decoded.to_string());
        }
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
    if let Some(cap) = regex::Regex::new(vqd_simple).ok()?.captures(html) {
        if let Ok(decoded) = urlencoding::decode(&cap[1]) {
            return Some(decoded.to_string());
        }
    }
    None
}

async fn get_results_with_vqd(
    client: &Client,
    query: &str,
    vqd: &str,
    headers: &HeaderMap,
) -> Result<String, reqwest::Error> {
    let ddg_url = format!(
        "https://html.duckduckgo.com/html/?q={}&vqd={}",
        urlencoding::encode(query),
        vqd
    );
    client
        .get(&ddg_url)
        .headers(headers.clone())
        .send()
        .await?
        .text()
        .await
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
    client: &Client,
    do_fetch_content: bool,
    headers: &HeaderMap,
) -> Json<Vec<SearchResult>> {
    let body_selector = Selector::parse("div.result__body").unwrap();
    #[derive(Clone)]
    struct RawResult {
        title: String,
        url: String,
        snippet: String,
    }

    let raw: Vec<RawResult> = {
        let document = Html::parse_document(html);
        let count = document.select(&body_selector).count();
        tracing::info!("Found {} result bodies", count);

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
            fetch_content(client, &raw.url, headers).await
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
    Json(results)
}

async fn fetch_content(client: &Client, url: &str, headers: &HeaderMap) -> String {
    // ── Phase 1: fetch with browser headers, capture Set-Cookie ──
    let mut cookie_header = String::new();

    let mut first_headers = headers.clone();
    if !cookie_header.is_empty() {
        first_headers.insert(
            HeaderName::from_static("cookie"),
            cookie_header.parse().unwrap(),
        );
    }

    let res = match client.get(url).headers(first_headers).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::info!("Fetch error for {}: {}", url, e);
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
        match client.get(url).headers(retry_headers).send().await {
            Ok(r) => {
                let s = r.status();
                let h = r.text().await.unwrap_or_default();
                (s, h)
            }
            Err(e) => {
                tracing::info!("Fetch retry error for {}: {}", url, e);
                (status, html)
            }
        }
    } else {
        (status, html)
    };

    if !status.is_success() {
        tracing::info!("Fetch {} for {}: {}", status, url, status);
        return String::new();
    }
    if final_html.len() < 100 {
        // Likely a redirect page or very short content
        return String::new();
    }
    extract_main_content(&final_html)
}
