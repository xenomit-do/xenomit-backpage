//! Egress proxy support (`docs/05-networking-and-proxies.md`).
//!
//! All backpage traffic a bot emits — login, `hangar_list`, maps/CDN —
//! must leave through *that bot's* assigned proxy so the whole session
//! shares one exit IP. [`ProxyConfig`] carries the endpoint plus optional
//! credentials; [`BackpageClient::with_proxy`](crate::BackpageClient::with_proxy)
//! and the `login_*_via` functions route every request through it.
//!
//! There is no direct fallback: a failed proxy request surfaces as
//! [`Error::Proxy`], the typed error `botd` maps to
//! `BotdError::ProxyFailure` (exit code 69). DNS resolves at the proxy —
//! `http`/`https` proxies receive `CONNECT host:port`/absolute-form
//! requests and `socks5h` keeps names remote.
//!
//! Credentials never reach `Debug`/`Display`: the password is `zeroize`d
//! and [`ProxyConfig::new`] moves `user:pass@` out of the URL into
//! [`ProxyConfig::auth`].

use crate::client::USER_AGENT;
use crate::error::{Error, Result};
use std::fmt;
use std::time::Duration;
use zeroize::Zeroizing;

/// Default echo endpoint for [`fetch_exit_ip`]; callers may substitute
/// any URL that returns the requester's IP as the response body.
pub const DEFAULT_EXIT_IP_URL: &str = "https://api.ipify.org/";

/// Schemes a backpage client accepts (matches `botd`'s `ProxySpec`).
const SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

/// Username/password credentials for an authenticating proxy. The
/// password is zeroized on drop and redacted in `Debug`.
#[derive(Clone)]
pub struct ProxyAuth {
    pub username: String,
    password: Zeroizing<String>,
}

impl ProxyAuth {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    /// Raw password — only at the authentication boundary.
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Per-bot egress proxy: `url` is `scheme://host:port` (`http`, `https`,
/// `socks5`, `socks5h`). `new` moves any `user:pass@` userinfo out of the
/// URL into [`ProxyConfig::auth`], so a stored config never carries the
/// password — even though `url` is a plain public field for
/// construction flexibility.
///
/// Constructed from `botd`'s `network.proxy` secret (`ProxySpec`) and
/// handed to [`BackpageClient::with_proxy`](crate::BackpageClient::with_proxy),
/// the `login_*_via` functions and `xenomit_packet_client`'s
/// `Session::connect_via*`.
#[derive(Clone)]
pub struct ProxyConfig {
    /// `scheme://host:port`. Userinfo supplied here is honored but should
    /// go through [`ProxyConfig::new`]/[`ProxyConfig::with_auth`], which
    /// strip it into `auth`.
    pub url: String,
    /// Proxy credentials, if the proxy authenticates.
    pub auth: Option<ProxyAuth>,
}

impl ProxyConfig {
    /// Parse `scheme://[user[:pass]@]host:port` — the URL stored in
    /// `url` is rebuilt without userinfo.
    pub fn new(url: impl Into<String>) -> Self {
        let url = url.into();
        match reqwest::Url::parse(&url) {
            Ok(mut parsed) => {
                let auth = (!parsed.username().is_empty())
                    .then(|| ProxyAuth::new(parsed.username(), parsed.password().unwrap_or("")));
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
                Self {
                    url: parsed.to_string(),
                    auth,
                }
            }
            Err(_) => Self { url, auth: None },
        }
    }

    /// Explicit-credentials constructor.
    pub fn with_auth(
        url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let mut cfg = Self::new(url);
        cfg.auth = Some(ProxyAuth::new(username, password));
        cfg
    }

    /// `scheme://host:port` with any userinfo removed — safe for logs.
    pub fn redacted_url(&self) -> String {
        match reqwest::Url::parse(&self.url) {
            Ok(mut parsed) => {
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
                parsed.to_string()
            }
            Err(_) => "<invalid proxy url>".into(),
        }
    }

    /// The URL's userinfo as `auth`, when the field isn't set — lets
    /// hand-built configs with embedded credentials still work.
    fn effective_auth(&self) -> Option<ProxyAuth> {
        self.auth.clone().or_else(|| {
            let parsed = reqwest::Url::parse(&self.url).ok()?;
            (!parsed.username().is_empty())
                .then(|| ProxyAuth::new(parsed.username(), parsed.password().unwrap_or("")))
        })
    }

    /// Build the `reqwest` proxy binding. Both `http`/`https` (CONNECT +
    /// absolute-form) and `socks5`/`socks5h` URLs are supported; DNS
    /// resolution happens at the proxy for CONNECT/socks5h.
    pub(crate) fn to_reqwest(&self) -> Result<reqwest::Proxy> {
        let mut url = reqwest::Url::parse(&self.url)
            .map_err(|e| Error::InvalidProxy(format!("{}: {e}", self.redacted_url())))?;
        if !SCHEMES.contains(&url.scheme()) {
            return Err(Error::InvalidProxy(format!(
                "{}: unsupported scheme {:?} (expected one of {SCHEMES:?})",
                self.redacted_url(),
                url.scheme()
            )));
        }
        let _ = url.set_username("");
        let _ = url.set_password(None);
        let auth = self.effective_auth();
        if url.scheme().starts_with("socks") {
            // SOCKS credentials travel in the URL for reqwest.
            if let Some(auth) = &auth {
                url.set_username(&auth.username)
                    .and_then(|_| url.set_password(Some(auth.password())))
                    .map_err(|_| {
                        Error::InvalidProxy("cannot embed credentials in socks URL".into())
                    })?;
            }
            return reqwest::Proxy::all(url).map_err(Error::Http);
        }
        let proxy = reqwest::Proxy::all(url).map_err(Error::Http)?;
        Ok(match auth {
            Some(auth) => proxy.basic_auth(&auth.username, auth.password()),
            None => proxy,
        })
    }
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("url", &self.redacted_url())
            .field("auth", &self.auth)
            .finish()
    }
}

impl fmt::Display for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted_url())
    }
}

/// The shared `reqwest` builder — same timeouts/UA/redirect policy the
/// direct client uses, plus the proxy when one is configured.
///
/// The `ProxyPolicy` is explicit per client (doc §5.1): `.no_proxy()`
/// pins a direct client to direct-only, so a stray `HTTP_PROXY` env var
/// can never silently reroute game egress.
pub(crate) fn http_client(proxy: Option<&ProxyConfig>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT);
    builder = match proxy {
        Some(proxy) => builder.proxy(proxy.to_reqwest()?),
        None => builder.no_proxy(),
    };
    Ok(builder.build()?)
}

/// Classify a `reqwest` send failure: when the client is proxied, a
/// transport-establishment error means the *egress path* failed (proxy
/// down, tunnel refused) — surface it as [`Error::Proxy`] so `botd`
/// exits 69 instead of crashing (or, worse, retrying direct).
pub(crate) fn classify(error: reqwest::Error, proxied: bool) -> Error {
    if proxied && error.is_connect() {
        Error::Proxy(error)
    } else {
        Error::Http(error)
    }
}

/// Companion to [`classify`] for rejections the proxy returns *inside*
/// HTTP rather than at transport level: on a forward-mode
/// (absolute-form) request a `407 Proxy Authentication Required` is an
/// ordinary response to reqwest, not an error. Call right after
/// `.send()` on proxied clients so it still maps to the fail-closed
/// egress class — never a credential/session error.
pub(crate) fn check_refused(resp: &reqwest::Response, proxied: bool) -> Result<()> {
    if proxied && resp.status() == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED {
        Err(Error::ProxyRefused(
            "HTTP 407 Proxy Authentication Required".into(),
        ))
    } else {
        Ok(())
    }
}

/// Exit-IP discovery (doc §5.4 health checks): GET `echo_url` through the
/// proxy and return the response body — the egress IP the game sees.
/// `echo_url` is pluggable; `None` uses [`DEFAULT_EXIT_IP_URL`].
///
/// ```no_run
/// # async fn demo() -> xenomit_backpage::Result<()> {
/// let proxy = xenomit_backpage::ProxyConfig::new("socks5h://127.0.0.1:1080");
/// let ip = xenomit_backpage::proxy::fetch_exit_ip(&proxy, None).await?;
/// println!("egress ip: {ip}");
/// # Ok(())
/// # }
/// ```
pub async fn fetch_exit_ip(proxy: &ProxyConfig, echo_url: Option<&str>) -> Result<String> {
    let http = http_client(Some(proxy))?;
    fetch_exit_ip_with(&http, echo_url).await
}

/// [`fetch_exit_ip`] over an already-proxied client — shared by
/// `BackpageClient::exit_ip` so session clients reuse their pool.
pub(crate) async fn fetch_exit_ip_with(
    http: &reqwest::Client,
    echo_url: Option<&str>,
) -> Result<String> {
    let url = echo_url.unwrap_or(DEFAULT_EXIT_IP_URL);
    let body = http
        .get(url)
        .send()
        .await
        .map_err(|e| classify(e, true))?
        .error_for_status()
        .map_err(Error::Http)?
        .text()
        .await
        .map_err(Error::Http)?;
    let ip = body.trim();
    if ip.is_empty() {
        return Err(Error::Parse(format!(
            "exit-ip endpoint {url} returned an empty body"
        )));
    }
    Ok(ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_moves_into_auth() {
        let cfg = ProxyConfig::new("http://alice:s3cret@proxy.local:3128");
        assert_eq!(cfg.url, "http://proxy.local:3128/");
        let auth = cfg.auth.as_ref().unwrap();
        assert_eq!(auth.username, "alice");
        assert_eq!(auth.password(), "s3cret");
    }

    #[test]
    fn debug_and_display_redact_credentials() {
        let cfg = ProxyConfig::with_auth("socks5h://proxy.local:1080", "u", "w0rd");
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("w0rd"), "{dbg}");
        assert!(dbg.contains("redacted"));
        let cfg = ProxyConfig::new("http://a:b@h:1");
        assert!(!format!("{cfg:?}").contains(":b@"));
        assert!(!format!("{cfg}").contains(":b@"));
    }

    #[test]
    fn schemes_and_reqwest_conversion() {
        for url in [
            "http://127.0.0.1:3128",
            "https://127.0.0.1:3129",
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
        ] {
            ProxyConfig::new(url)
                .to_reqwest()
                .unwrap_or_else(|e| panic!("{url}: {e}"));
        }
        assert!(matches!(
            ProxyConfig::new("ftp://127.0.0.1:21").to_reqwest(),
            Err(Error::InvalidProxy(_))
        ));
        assert!(matches!(
            ProxyConfig::new("not a url").to_reqwest(),
            Err(Error::InvalidProxy(_))
        ));
    }
}
