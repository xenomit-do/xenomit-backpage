# xenomit-backpage

Rust client for DarkOrbit's `*.darkorbit.com` Backpage — `dosid` session
login (password or sid), the `flashembed` launch contract (authenticated
FlashVars → `userID`, `sessionID`, `pid`, `mapID`), and the `ajax/*.php`
ops surface (hangar, auction, captain/P.E.T., shop, skylab, user).

```rust
let session = xenomit_backpage::login::login_with_credentials("user", "pass").await?;
let mut client = xenomit_backpage::BackpageClient::new(session)?;
xenomit_backpage::launch::load(&mut client).await?;   // fills flashvars
```

## Egress proxy (`docs/05-networking-and-proxies.md`)

A bot's *entire* game egress — login, `hangar_list`, `maps.php`, CDN —
must leave through **its** assigned proxy so the whole session shares
one exit IP. `src/proxy.rs` provides `ProxyConfig`/`ProxyAuth`; every
entry point has a `*_via` twin:

```rust
use xenomit_backpage::{BackpageClient, ProxyConfig};

let proxy = ProxyConfig::new("socks5h://user:pass@127.0.0.1:1080");
// or: ProxyConfig::with_auth("http://127.0.0.1:3128", "user", "pass")

// Credentials login through the proxy:
let session = xenomit_backpage::login::login_with_credentials_via("user", "pass", &proxy).await?;
// …or sid login: login_with_sid_via(server, sid, &proxy) — both have *_at_via
// twins for explicit base URLs (mock servers, headless daemons).

let mut client = BackpageClient::with_proxy(session, &proxy)?;
xenomit_backpage::launch::load(&mut client).await?;
```

| URL scheme | reqwest transport | DNS |
|---|---|---|
| `http` | CONNECT for `https` targets, absolute-form for `http` | at the proxy |
| `https` | same, over TLS to the proxy | at the proxy |
| `socks5h` | SOCKS5 tunnel | at the proxy |
| `socks5` | SOCKS5 tunnel | locally (scheme semantics — prefer `socks5h`) |

Properties:

- **Fail closed**: when a proxied request cannot establish egress
  (proxy unreachable, CONNECT refused), `Error::Proxy` is returned —
  `Error::is_proxy_failure()` marks it. `botd` maps it to
  `BotdError::ProxyFailure` → exit 69; nothing ever retried directly.
  `Error::InvalidProxy` is the config-class twin (bad URL/scheme →
  exit 64 territory).
- **Timeouts**: same policy as the direct client (30 s connect,
  30 s request) — configured once in `proxy::http_client`.
- **Credentials**: `ProxyAuth.password` is `zeroize`d;
  `Debug`/`Display` of `ProxyConfig`/`ProxyAuth` redact credentials and
  `ProxyConfig::new` moves `user:pass@` out of the URL into `auth`.
- **DNS**: `CONNECT`/absolute-form/`socks5h` all resolve names at the
  proxy, so game hostnames never hit the pod resolver.

### Exit-IP discovery

`proxy::fetch_exit_ip(&proxy, url)` — or `client.exit_ip(url)` on an
already-proxied client — GETs an echo endpoint *through the proxy* and
returns the body (the egress IP). The URL is pluggable
(`None` → `https://api.ipify.org/`); the orchestrator's health checks
use it to detect silent proxy rotation or a leaked non-proxy exit.

### Wiring for `xenomit-botd` / `xenomit-bot-runtime`

`botd` resolves `network.proxy` into a `ProxySpec`
(`scheme, host, port, username, password`). Convert it once per bot and
share the `Option<ProxyConfig>` between the backpage login and the
packet session:

```rust
let proxy: Option<ProxyConfig> = spec.map(|s| {
    let mut c = ProxyConfig::new(format!("{}://{}:{}", s.scheme, s.host, s.port));
    if let Some(u) = &s.username {
        let pass = s.password.as_ref().map(|p| p.reveal()).unwrap_or("");
        c.auth = Some(ProxyAuth::new(u, pass));
    }
    c
});

let session = match &proxy {
    Some(p) => login::login_with_credentials_via(user, pass, p).await,
    None => login::login_with_credentials(user, pass).await,
};
let mut client = match &proxy {
    Some(p) => BackpageClient::with_proxy(session?, p),
    None => BackpageClient::new(session?),
};
```

Rules: with `network.require_proxy`, refuse to arm when `proxy` is
`None` (exit 69); map `Error::Proxy` to `BotdError::ProxyFailure` —
never fall back to a direct client; pair it with
`xenomit_packet_client::Session::connect_via*` for the game socket (see
that crate's README) so backpage *and* game TCP share the exit IP.
Infra traffic (panel, gateway, OTLP) keeps using its own non-proxied
clients — the proxy is per-client, never a global env var.
