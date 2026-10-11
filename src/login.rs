use crate::client::url_encode;
use crate::error::{Error, Result};
use crate::proxy::ProxyConfig;
use crate::session::Session;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

fn login_form_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""bgcdw_login_form" action="(.*)""#).unwrap())
}

/// Session id cookie collected from a `set-cookie` header.
struct DosidCookie {
    value: String,
    domain: String,
    /// `scheme://host[:port]` of the response that issued the cookie —
    /// becomes the session's `instance_base` when it differs from the
    /// derived `https://{server}` default (mock servers over plain HTTP).
    origin: String,
}

/// Minimal cookie jar for the login redirect chain: `name=value` pairs keyed by name.
type CookieJar = HashMap<String, String>;

/// frontpage scrape → login form POST → dosid cookie.
///
/// the `dosid` cookie is set on a `*\d+.*` domain (e.g. `int1.darkorbit.com`);
/// that domain becomes the session's server.
pub async fn login_with_credentials(username: &str, password: &str) -> Result<Session> {
    login_on_domain(username, password, "www", None).await
}

/// [`login_with_credentials`] routed through the bot's egress proxy.
pub async fn login_with_credentials_via(
    username: &str,
    password: &str,
    proxy: &ProxyConfig,
) -> Result<Session> {
    login_on_domain(username, password, "www", Some(proxy)).await
}

/// Same flow against an explicit front base URL instead of
/// `https://<domain>.darkorbit.com/` — used by headless daemons and tests
/// against mock Backpage servers.
pub async fn login_with_credentials_at(
    front_base_url: &str,
    username: &str,
    password: &str,
) -> Result<Session> {
    login_at_base(
        &format!("{}/", front_base_url.trim_end_matches('/')),
        username,
        password,
        None,
    )
    .await
}

/// [`login_with_credentials_at`] routed through the bot's egress proxy.
pub async fn login_with_credentials_at_via(
    front_base_url: &str,
    username: &str,
    password: &str,
    proxy: &ProxyConfig,
) -> Result<Session> {
    login_at_base(
        &format!("{}/", front_base_url.trim_end_matches('/')),
        username,
        password,
        Some(proxy),
    )
    .await
}

async fn login_on_domain(
    username: &str,
    password: &str,
    domain: &str,
    proxy: Option<&ProxyConfig>,
) -> Result<Session> {
    login_at_base(
        &format!("https://{domain}.darkorbit.com/"),
        username,
        password,
        proxy,
    )
    .await
}

async fn login_at_base(
    front_url: &str,
    username: &str,
    password: &str,
    proxy: Option<&ProxyConfig>,
) -> Result<Session> {
    let http = crate::proxy::http_client(proxy)?;
    let proxied = proxy.is_some();

    let front_page = get_text(&http, front_url, proxied).await?;

    if front_page.contains("class=\"bgcdw_captcha\"") {
        return Err(Error::CaptchaRequired);
    }

    let login_url = login_form_re()
        .captures(&front_page)
        .map(|c| c[1].replace("&amp;", "&"))
        .ok_or_else(|| Error::Parse("no bgcdw_login_form on frontpage".into()))?;

    // POST the login form, following redirects manually so we see every
    // set-cookie along the way
    let mut jar = CookieJar::new();
    let mut dosid: Option<DosidCookie> = None;
    let mut url = login_url;
    let body = format!(
        "username={}&password={}",
        url_encode(username),
        url_encode(password)
    );
    let mut request = http
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body);

    for _ in 0..10 {
        // max 10 times redirection
        if !jar.is_empty() {
            request = request
                .try_clone()
                .unwrap()
                .header("Cookie", jar_header(&jar));
        }
        let resp = request
            .send()
            .await
            .map_err(|e| crate::proxy::classify(e, proxied))?;
        crate::proxy::check_refused(&resp, proxied)?;
        collect_cookies(&resp, &mut jar, &mut dosid);
        if resp.status().is_redirection()
            && let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok())
        {
            url = resolve_url(&url, loc);
            request = http.get(&url);
            continue;
        }
        break;
    }

    let dosid = dosid.ok_or(Error::WrongCredentials)?;
    let domain = dosid.domain.trim_start_matches('.').to_string();
    let mut session = Session::new(dosid.value, domain);
    if dosid.origin != format!("https://{}", session.server) {
        session.set_instance_base(Some(dosid.origin));
    }
    Ok(session)
}

/// `login --server X --sid Y` path — validate the sid by loading the game contract.
pub async fn login_with_sid(server: &str, sid: &str) -> Result<Session> {
    sid_login(Session::new(sid, server), None).await
}

/// [`login_with_sid`] routed through the bot's egress proxy.
pub async fn login_with_sid_via(server: &str, sid: &str, proxy: &ProxyConfig) -> Result<Session> {
    sid_login(Session::new(sid, server), Some(proxy)).await
}

/// [`login_with_sid`] against an explicit instance base URL
/// (`scheme://host[:port]`, e.g. a mock backpage) — companion to
/// [`login_with_credentials_at`]. `session.server` records the base's
/// host verbatim so cookie-scoped requests keep matching it.
pub async fn login_with_sid_at(instance_base_url: &str, sid: &str) -> Result<Session> {
    sid_login(sid_at_session(instance_base_url, sid), None).await
}

/// [`login_with_sid_at`] routed through the bot's egress proxy.
pub async fn login_with_sid_at_via(
    instance_base_url: &str,
    sid: &str,
    proxy: &ProxyConfig,
) -> Result<Session> {
    sid_login(sid_at_session(instance_base_url, sid), Some(proxy)).await
}

fn sid_at_session(instance_base_url: &str, sid: &str) -> Session {
    let host = reqwest::Url::parse(instance_base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| instance_base_url.trim_end_matches('/').to_string());
    let mut session = Session::new(sid, &host);
    session.server = host;
    session.set_instance_base(Some(instance_base_url.to_string()));
    session
}

async fn sid_login(session: Session, proxy: Option<&ProxyConfig>) -> Result<Session> {
    let mut client = match proxy {
        Some(proxy) => crate::client::BackpageClient::with_proxy(session, proxy)?,
        None => crate::client::BackpageClient::new(session)?,
    };
    crate::launch::load(&mut client).await?;
    Ok(client.session().clone())
}

async fn get_text(http: &reqwest::Client, url: &str, proxied: bool) -> Result<String> {
    let mut jar = CookieJar::new();
    let mut url = url.to_string();
    for _ in 0..10 {
        let mut req = http.get(&url);
        if !jar.is_empty() {
            req = req.header("Cookie", jar_header(&jar));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| crate::proxy::classify(e, proxied))?;
        crate::proxy::check_refused(&resp, proxied)?;
        collect_cookies(&resp, &mut jar, &mut None);
        if resp.status().is_redirection()
            && let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok())
        {
            url = resolve_url(&url, loc);
            continue;
        }
        return body_text(&resp.bytes().await?);
    }
    Err(Error::Parse("too many redirects".into()))
}

/// The backpage serves gzip without a `Content-Encoding` header — sniff the
/// magic bytes like `Http.getInputStream` does.
fn body_text(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(bytes), &mut out)
            .map_err(|e| Error::Parse(format!("gzip decode: {e}")))?;
        Ok(out)
    } else {
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn collect_cookies(resp: &reqwest::Response, jar: &mut CookieJar, dosid: &mut Option<DosidCookie>) {
    for value in resp.headers().get_all("set-cookie").iter() {
        let Ok(raw) = value.to_str() else { continue };
        let mut parts = raw.split(';').map(str::trim);
        let Some(pair) = parts.next() else { continue };
        let Some((name, val)) = pair.split_once('=') else {
            continue;
        };
        jar.insert(name.to_string(), val.to_string());
        if name.eq_ignore_ascii_case("dosid") {
            let domain = parts
                .find_map(|p| {
                    p.strip_prefix("Domain=")
                        .or_else(|| p.strip_prefix("domain="))
                })
                .unwrap_or_default()
                .to_string();
            // The session cookie lives on a numeric instance domain like `int1.darkorbit.com`.
            let host = domain.clone();
            let effective = if host.is_empty() {
                resp.url().host_str().unwrap_or_default().to_string()
            } else {
                host
            };
            if effective.chars().any(|c| c.is_ascii_digit()) {
                let mut origin = format!("{}://{}", resp.url().scheme(), effective);
                if let Some(port) = resp.url().port() {
                    origin.push_str(&format!(":{port}"));
                }
                *dosid = Some(DosidCookie {
                    value: val.to_string(),
                    domain: effective,
                    origin,
                });
            }
        }
    }
}

fn jar_header(jar: &CookieJar) -> String {
    jar.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn resolve_url(base: &str, location: &str) -> String {
    if location.starts_with("http") {
        return location.to_string();
    }
    if let Ok(base_url) = reqwest::Url::parse(base)
        && let Ok(joined) = base_url.join(location)
    {
        return joined.to_string();
    }
    location.to_string()
}
