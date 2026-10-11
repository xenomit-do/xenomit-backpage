#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("failed to parse response: {0}")]
    Parse(String),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error(
        "login requires solving a captcha; extract 'dosid' from a browser session and use --server/--sid instead"
    )]
    CaptchaRequired,

    #[error("wrong or expired credentials")]
    WrongCredentials,

    #[error("session is invalid or expired (sid rejected by server)")]
    SessionExpired,

    #[error("session is incomplete: sid, server and user_id are required")]
    InvalidSession,

    /// The proxy URL/scheme is unusable — a config-class failure
    /// (`botd`: `InvalidConfig`, exit 64), raised before any request.
    #[error("invalid proxy configuration: {0}")]
    InvalidProxy(String),

    /// A proxied request failed while establishing egress — proxy
    /// unreachable, tunnel refused. Fail closed: `botd` maps this to
    /// `BotdError::ProxyFailure` (exit 69); never retried directly.
    #[error("proxy egress failed: {0}")]
    Proxy(#[source] reqwest::Error),

    /// The proxy itself rejected the request — a forward-mode HTTP 407
    /// that reqwest surfaces as a normal response rather than a
    /// transport error. Same egress-failure class as [`Error::Proxy`]:
    /// `botd` maps it to `BotdError::ProxyFailure` (exit 69).
    #[error("proxy refused the request: {0}")]
    ProxyRefused(String),

    #[error("api error: {0}")]
    Api(String),
}

impl Error {
    /// Whether this failure belongs to the proxy egress path —
    /// `botd` maps it to `BotdError::ProxyFailure` (exit 69) rather
    /// than a retry. Config-class proxy problems report
    /// [`Error::InvalidProxy`] separately.
    pub fn is_proxy_failure(&self) -> bool {
        matches!(self, Error::Proxy(_) | Error::ProxyRefused(_))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
