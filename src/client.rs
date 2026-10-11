use crate::error::{Error, Result};
use crate::proxy::ProxyConfig;
use crate::session::Session;
use regex::Regex;
use reqwest::StatusCode;
use std::io::Read;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const USER_AGENT: &str = "BigpointClient/1.6.9";
const SHOP_PATH: &str = "ajax/shop.php";
/// Actions pinged to check whether the sid is still alive
const SID_CHECK_ACTIONS: [&str; 3] = [
    "internalDock",
    "internalDock&tpl=internalDockAmmo",
    "internalSkylab",
];
/// After any shop.php call, the sid probe buys ammo instead.
const SHOP_TIMER: Duration = Duration::from_secs(20 * 60);

fn reload_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new("reloadToken=([^\"]+)").unwrap())
}

/// Java `URLEncoder.encode` equivalent: alphanumerics and `-_.*` pass through,
/// space becomes `+`, everything else is percent-encoded (UTF-8, uppercase hex).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Java `URLDecoder.decode` equivalent.
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => out.push(b),
                    Err(_) => out.push(b'%'),
                }
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One form parameter, mirroring `Http.setParam` / `setRawParam`.
#[derive(Debug, Clone)]
pub struct Param {
    pub key: String,
    pub value: String,
    /// `false` = URL-encode both key and value (`setParam`); `true` = as-is (`setRawParam`).
    pub raw: bool,
}

impl Param {
    pub fn new(key: impl ToString, value: impl ToString) -> Self {
        Param {
            key: key.to_string(),
            value: value.to_string(),
            raw: false,
        }
    }

    pub fn raw(key: impl ToString, value: impl ToString) -> Self {
        Param {
            key: key.to_string(),
            value: value.to_string(),
            raw: true,
        }
    }

    fn render(&self) -> String {
        if self.raw {
            format!("{}={}", self.key, self.value)
        } else {
            format!("{}={}", url_encode(&self.key), url_encode(&self.value))
        }
    }
}

/// Status of the session id, mirroring `BackpageManager.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidStatus {
    Unknown,
    NoSid,
    Valid,
    Error,
    Invalid,
}

impl SidStatus {
    fn of(code: StatusCode) -> Self {
        match code {
            StatusCode::OK => SidStatus::Valid,
            StatusCode::FOUND => SidStatus::Invalid,
            _ => SidStatus::Error,
        }
    }
}

impl std::fmt::Display for SidStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SidStatus::Unknown => "?",
            SidStatus::NoSid => "--",
            SidStatus::Valid => "OK",
            SidStatus::Error => "ERR",
            SidStatus::Invalid => "KO",
        })
    }
}

/// Raw response from a backpage request.
#[derive(Debug)]
pub struct Response {
    pub status: StatusCode,
    pub body: String,
    /// Final URL after any client-side redirect handling (none — redirects are off).
    pub url: String,
}

/// Shared, rate-limited HTTP client bound to one session.
/// Equivalent of `BackpageManager`'s connection helpers.
pub struct BackpageClient {
    pub(crate) http: reqwest::Client,
    session: Session,
    /// Egress proxy every request is routed through (`None` = direct).
    proxy: Option<ProxyConfig>,
    last_request: Mutex<Instant>,
    /// Armed by any `ajax/shop.php` call; makes `check_sid` use the live ammo probe.
    last_shop_request: Mutex<Option<Instant>>,
    pub default_min_wait: Duration,
}

impl BackpageClient {
    pub fn new(session: Session) -> Result<Self> {
        Self::build(session, None)
    }

    /// [`BackpageClient::new`] with every request routed through the
    /// bot's egress proxy (`docs/05-networking-and-proxies.md`): HTTP(S)
    /// uses CONNECT/absolute-form, `socks5h` keeps DNS remote. There is
    /// no direct fallback — egress failures surface as [`Error::Proxy`].
    pub fn with_proxy(session: Session, proxy: &ProxyConfig) -> Result<Self> {
        Self::build(session, Some(proxy))
    }

    fn build(session: Session, proxy: Option<&ProxyConfig>) -> Result<Self> {
        let http = crate::proxy::http_client(proxy)?;
        Ok(BackpageClient {
            http,
            session,
            proxy: proxy.cloned(),
            last_request: Mutex::new(Instant::now() - Duration::from_secs(60)),
            last_shop_request: Mutex::new(None),
            default_min_wait: Duration::from_millis(1000),
        })
    }

    /// The egress proxy this client was built with, if any.
    pub fn proxy(&self) -> Option<&ProxyConfig> {
        self.proxy.as_ref()
    }

    fn proxied(&self) -> bool {
        self.proxy.is_some()
    }

    /// Exit-IP discovery (doc §5.4): GET an echo endpoint through this
    /// client's proxy and return the reported egress IP. `echo_url` is
    /// pluggable (`None` = [`crate::proxy::DEFAULT_EXIT_IP_URL`]).
    /// Requires a proxied client — a direct client's "exit IP" would be
    /// the host's own and defeat the check.
    pub async fn exit_ip(&self, echo_url: Option<&str>) -> Result<String> {
        if self.proxy.is_none() {
            return Err(Error::InvalidProxy(
                "exit_ip requires a client built with_proxy".into(),
            ));
        }
        crate::proxy::fetch_exit_ip_with(&self.http, echo_url).await
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub(crate) async fn wait_rate_limit(&self, min_wait: Duration) {
        let mut last = self.last_request.lock().await;
        let wait = *last + min_wait - Instant::now();
        if wait > Duration::ZERO {
            tokio::time::sleep(wait).await;
        }
        *last = Instant::now();
    }

    /// GET `instance + path`. Params are appended as a query string only when the
    /// path has no `?` — same rule as `Http.getUrl()`.
    pub async fn get(&self, path: &str, params: &[Param]) -> Result<Response> {
        self.get_wait(path, params, self.default_min_wait).await
    }

    pub async fn get_wait(
        &self,
        path: &str,
        params: &[Param],
        min_wait: Duration,
    ) -> Result<Response> {
        self.session.require_valid()?;
        self.wait_rate_limit(min_wait).await;

        let mut url = format!("{}{}", self.session.instance(), path);
        if !params.is_empty() && !url.contains('?') {
            url.push('?');
            url.push_str(&render_params(params));
        }
        self.send(self.http.get(&url)).await
    }

    /// POST `instance + path` with a urlencoded form body.
    pub async fn post(&self, path: &str, params: &[Param]) -> Result<Response> {
        self.post_wait(path, params, self.default_min_wait).await
    }

    pub async fn post_wait(
        &self,
        path: &str,
        params: &[Param],
        min_wait: Duration,
    ) -> Result<Response> {
        self.session.require_valid()?;
        if path.to_lowercase().contains(SHOP_PATH) {
            *self.last_shop_request.lock().await = Some(Instant::now());
        }
        self.wait_rate_limit(min_wait).await;

        let url = format!("{}{}", self.session.instance(), path);
        let body = render_params(params);
        self.send(
            self.http
                .post(&url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body),
        )
        .await
    }

    pub(crate) async fn send(&self, req: reqwest::RequestBuilder) -> Result<Response> {
        let resp = req
            .header("Cookie", format!("dosid={}", self.session.sid))
            .send()
            .await
            .map_err(|e| crate::proxy::classify(e, self.proxied()))?;
        crate::proxy::check_refused(&resp, self.proxied())?;
        let status = resp.status();
        let url = resp.url().to_string();
        let bytes = resp.bytes().await?;
        // The backpage sometimes serves gzip without a Content-Encoding header;
        // sniff the magic bytes like `Http.getInputStream` does.
        let body = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut out = String::new();
            flate2::read::GzDecoder::new(&bytes[..])
                .read_to_string(&mut out)
                .map_err(|e| Error::Parse(format!("gzip decode: {e}")))?;
            out
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Ok(Response { status, body, url })
    }

    /// Mirrors `BackpageManager.checkSidValid` probe logic.
    pub async fn check_sid(&self) -> SidStatus {
        if self.session.sid.is_empty() || self.session.server.is_empty() {
            return SidStatus::NoSid;
        }
        let shop_armed = self
            .last_shop_request
            .lock()
            .await
            .is_some_and(|t| t.elapsed() < SHOP_TIMER);

        if shop_armed {
            self.shop_probe().await
        } else {
            let idx = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as usize)
                .unwrap_or(0)
                ^ std::process::id() as usize)
                % SID_CHECK_ACTIONS.len();
            match self
                .get_wait(
                    &format!("indexInternal.es?action={}", SID_CHECK_ACTIONS[idx]),
                    &[],
                    Duration::ZERO,
                )
                .await
            {
                Ok(resp) => SidStatus::of(resp.status),
                Err(_) => SidStatus::Error,
            }
        }
    }

    /// Live probe: buy 1x lcb-10 — a 200 containing `redirectToLogin` means the sid is dead.
    async fn shop_probe(&self) -> SidStatus {
        let status = match self
            .get_wait(
                "indexInternal.es?action=internalDock&tpl=internalDockAmmo",
                &[],
                Duration::from_millis(3000),
            )
            .await
        {
            Ok(r) => SidStatus::of(r.status),
            Err(_) => return SidStatus::Error,
        };
        if status != SidStatus::Valid {
            return status;
        }
        let params = [
            Param::new("action", "purchase"),
            Param::new("category", "battery"),
            Param::new("itemId", "ammunition_laser_lcb-10"),
            Param::new("amount", "1"),
            Param::new("selectedName", ""),
        ];
        let url = format!("{}{}", self.session.instance(), SHOP_PATH);
        let req = self
            .http
            .post(&url)
            .header(
                "Referer",
                format!(
                    "{}indexInternal.es?action=internalDock&tpl=internalDockAmmo",
                    self.session.instance()
                ),
            )
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(render_params(&params));
        self.wait_rate_limit(Duration::ZERO).await;
        *self.last_shop_request.lock().await = Some(Instant::now());
        match req
            .header("Cookie", format!("dosid={}", self.session.sid))
            .send()
            .await
            .map_err(|e| crate::proxy::classify(e, self.proxied()))
        {
            Ok(resp) => {
                let status = SidStatus::of(resp.status());
                if status != SidStatus::Valid {
                    return status;
                }
                match resp.text().await {
                    Ok(t) if t.contains("redirectToLogin") => SidStatus::Invalid,
                    Ok(_) => SidStatus::Valid,
                    Err(_) => SidStatus::Error,
                }
            }
            Err(_) => SidStatus::Error,
        }
    }

    /// `reloadToken=...` embedded in backpage HTML, required for mutating calls.
    pub fn reload_token(body: &str) -> Option<String> {
        reload_token_re().captures(body).map(|c| c[1].to_string())
    }

    pub async fn fetch_reload_token(&self, path: &str) -> Result<String> {
        let resp = self.get(path, &[]).await?;
        Self::reload_token(&resp.body).ok_or_else(|| Error::Parse("no reloadToken in page".into()))
    }

    /// POST helper for the `ajax/*.php` JSON endpoints: checks `result` field.
    pub(crate) async fn ajax(&self, path: &str, params: &[Param]) -> Result<serde_json::Value> {
        let resp = self.post(path, params).await?;
        if resp.status == StatusCode::FOUND || resp.body.contains("redirectToLogin") {
            return Err(Error::SessionExpired);
        }
        let json: serde_json::Value = serde_json::from_str(&resp.body)
            .map_err(|e| Error::Parse(format!("{e}: {}", truncate(&resp.body))))?;
        Ok(json)
    }
}

fn render_params(params: &[Param]) -> String {
    params
        .iter()
        .map(Param::render)
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn truncate(s: &str) -> &str {
    if s.len() > 300 { &s[..300] } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_java() {
        assert_eq!(url_encode("a b"), "a+b");
        assert_eq!(url_encode("a+b&c=d"), "a%2Bb%26c%3Dd");
        assert_eq!(url_encode("x*y-z_w.q"), "x*y-z_w.q");
        assert_eq!(url_encode("ö"), "%C3%B6");
    }

    #[test]
    fn decode_roundtrip() {
        assert_eq!(url_decode("a+b%26c%3D"), "a b&c=");
        assert_eq!(url_decode("%C3%B6"), "ö");
    }
}
