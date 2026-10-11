//! Game startup that begins on the backpage map page.
//!
//! The inline script calls `flashembed` with a JavaScript options object (the
//! `onFail` value is a function, so the object is not JSON) and a JSON
//! FlashVar object. That FlashVar object is the launch contract. The same
//! script POSTs `flashAPI/loadingScreen.php` with `setClientBrowserConfig`
//! and ignores the response.

use crate::client::{BackpageClient, url_encode};
use crate::error::{Error, Result};
use regex::Regex;
use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

/// Read the map page script, keep its `flashembed` contract, and POST the
/// browser-config notice that script sends to `flashAPI/loadingScreen.php`.
pub async fn load(client: &mut BackpageClient) -> Result<()> {
    let page = client
        .get("indexInternal.es?action=internalMapRevolution", &[])
        .await?;
    if page.status.is_redirection() || page.body.contains("redirectToLogin") {
        return Err(Error::SessionExpired);
    }
    let embed = parse_embed(&html_unescape(&page.body)).ok_or_else(|| {
        Error::Parse(
            "map page did not contain a preloader embed; the dosid is probably expired".into(),
        )
    })?;

    let mut vars = embed.flashvars;
    let loading = post_loading_screen(client).await?;
    if loading.status.is_redirection() || loading.body.contains("redirectToLogin") {
        return Err(Error::SessionExpired);
    }
    if let Some(authenticated) = parse_authenticated_flashvars(&html_unescape(&loading.body)) {
        for (key, value) in authenticated {
            if !value.is_empty() {
                vars.insert(key, value);
            }
        }
    }
    for key in ["userID", "sessionID"] {
        match vars.get(key) {
            Some(value) if !value.is_empty() => {}
            _ => {
                return Err(Error::Parse(format!(
                    "authenticated Flash contract is missing {key}"
                )));
            }
        }
    }

    let movie = resolve_movie(&client.session().instance(), &embed.src);
    client
        .session_mut()
        .set_stage_size(embed.width, embed.height);
    client.session_mut().set_preloader(movie, vars);
    Ok(())
}

/// Download the preloader (or any later movie URL) with the session cookie.
/// Gzip bodies are inflated. A CWS/ZWS SWF is left intact.
pub async fn fetch_movie(client: &BackpageClient, url: &str) -> Result<Vec<u8>> {
    client.session().require_valid()?;
    client.wait_rate_limit(Duration::ZERO).await;
    let response = client
        .http
        .get(url)
        .header("Cookie", format!("dosid={}", client.session().sid))
        .send()
        .await
        .map_err(|e| crate::proxy::classify(e, client.proxy().is_some()))?;
    crate::proxy::check_refused(&response, client.proxy().is_some())?;
    if response.status().is_redirection() {
        return Err(Error::SessionExpired);
    }
    if !response.status().is_success() {
        return Err(Error::Parse(format!(
            "download returned HTTP {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .map_err(|e| Error::Parse(format!("gzip decode: {e}")))?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

/// The `jQuery.post` in the map script. Its callback is empty; the body is
/// the Flash plugin version and browser name, not the launch contract.
async fn post_loading_screen(client: &BackpageClient) -> Result<crate::client::Response> {
    client.session().require_valid()?;
    client.wait_rate_limit(client.default_min_wait).await;
    let url = format!("{}flashAPI/loadingScreen.php", client.session().instance());
    let referer = format!(
        "{}indexInternal.es?action=internalMapRevolution",
        client.session().instance()
    );
    let params = url_encode(r#"["jQuery.flashEmbed","11.2","Unknown"]"#);
    let body = format!("action=setClientBrowserConfig&params={params}&isEncodeMessage=0");
    client
        .send(
            client
                .http
                .post(&url)
                .header("Referer", &referer)
                .header("X-Requested-With", "XMLHttpRequest")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body),
        )
        .await
}

struct EmbedConfig {
    src: String,
    width: i32,
    height: i32,
    flashvars: HashMap<String, String>,
}

fn html_unescape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let entity = &rest[start..];
        let Some(end) = entity.find(';') else {
            out.push_str(entity);
            return out;
        };
        match &entity[..=end] {
            "&amp;" => out.push('&'),
            "&quot;" => out.push('"'),
            "&#34;" => out.push('"'),
            "&apos;" => out.push('\''),
            "&#39;" => out.push('\''),
            "&lt;" => out.push('<'),
            "&gt;" => out.push('>'),
            other => out.push_str(other),
        }
        rest = &entity[end + 1..];
    }
    out.push_str(rest);
    out
}

fn json_values(page: &str) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    let mut rest = page;
    while let Some(offset) = rest.find('{') {
        let candidate = &rest[offset..];
        let mut stream =
            serde_json::Deserializer::from_str(candidate).into_iter::<serde_json::Value>();
        match stream.next() {
            Some(Ok(value)) => {
                let consumed = stream.byte_offset().max(1);
                values.push(value);
                rest = &candidate[consumed..];
            }
            _ => rest = &candidate[1..],
        }
    }
    values
}

fn json_to_flashvar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => Some(String::new()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => None,
    }
}

fn object_to_vars(value: &serde_json::Value) -> Option<HashMap<String, String>> {
    let object = value.as_object()?;
    let mut vars = HashMap::new();
    for (key, item) in object {
        if let Some(text) = json_to_flashvar(item) {
            vars.insert(key.clone(), text);
        }
    }
    if vars.is_empty() { None } else { Some(vars) }
}

fn is_flashvar_object(vars: &HashMap<String, String>) -> bool {
    [
        "cdn",
        "mapID",
        "userID",
        "sessionID",
        "factionID",
        "gameclientHash",
    ]
    .into_iter()
    .any(|key| vars.contains_key(key))
}

/// First `flashembed` call. The options argument is JavaScript, not JSON.
fn parse_embed(page: &str) -> Option<EmbedConfig> {
    let mut rest = page;
    while let Some(index) = rest.find("flashembed(") {
        let after = &rest[index + "flashembed(".len()..];
        if let Some(embed) = parse_flashembed_call(after) {
            return Some(embed);
        }
        rest = &rest[index + "flashembed(".len()..];
    }
    None
}

fn parse_flashembed_call(after_paren: &str) -> Option<EmbedConfig> {
    let args = split_call_args(after_paren)?;
    if args.len() < 3 {
        return None;
    }
    let config = &args[1];
    let src = string_field(config, "src")?;
    let width = dimension_field(config, "width")?;
    let height = dimension_field(config, "height")?;
    let flashvars = parse_var_object(&args[2])?;
    if !is_flashvar_object(&flashvars) {
        return None;
    }
    Some(EmbedConfig {
        src: src.replace("\\/", "/"),
        width,
        height,
        flashvars,
    })
}

/// Arguments of one call, respecting quotes, brackets, and braces.
fn split_call_args(input: &str) -> Option<Vec<String>> {
    let bytes = input.as_bytes();
    let mut args = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut braces = 0i32;
    let mut brackets = 0i32;
    let mut parens = 0i32;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == open {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'{' => braces += 1,
            b'}' => braces -= 1,
            b'[' => brackets += 1,
            b']' => brackets -= 1,
            b'(' => parens += 1,
            b')' if braces == 0 && brackets == 0 && parens == 0 => {
                args.push(input[start..index].trim().to_string());
                return Some(args);
            }
            b')' => parens -= 1,
            b',' if braces == 0 && brackets == 0 && parens == 0 => {
                args.push(input[start..index].trim().to_string());
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn string_field(object: &str, key: &str) -> Option<String> {
    let pattern = format!(r#""{key}"\s*:\s*"([^"]*)""#);
    Regex::new(&pattern)
        .ok()?
        .captures(object)
        .map(|caps| caps[1].to_string())
}

fn dimension_field(object: &str, key: &str) -> Option<i32> {
    let pattern = format!(r#""{key}"\s*:\s*"?(\d+)"#);
    Regex::new(&pattern)
        .ok()?
        .captures(object)
        .and_then(|caps| caps[1].parse().ok())
}

fn parse_var_object(raw: &str) -> Option<HashMap<String, String>> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(vars) = object_to_vars(&value)
    {
        return Some(vars);
    }
    let pattern = Regex::new(r#""([^"]+)"\s*:\s*"([^"]*)""#).ok()?;
    let mut vars = HashMap::new();
    for caps in pattern.captures_iter(raw) {
        vars.insert(caps[1].to_string(), caps[2].to_string());
    }
    (!vars.is_empty()).then_some(vars)
}

/// Authenticated vars from the loading-screen response: a JSON object, or the
/// Flash object the page builds from that response.
fn parse_authenticated_flashvars(body: &str) -> Option<HashMap<String, String>> {
    let trimmed = body.trim();
    if trimmed.starts_with('{')
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(vars) = object_to_vars(&value)
        && vars.contains_key("sessionID")
    {
        return Some(vars);
    }

    json_values(body)
        .into_iter()
        .rev()
        .find_map(|value| {
            let vars = object_to_vars(&value)?;
            vars.contains_key("sessionID").then_some(vars)
        })
        .or_else(|| parse_flashvar_attribute(body))
}

fn parse_flashvar_attribute(body: &str) -> Option<HashMap<String, String>> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let pattern = RE.get_or_init(|| {
        Regex::new(
            r#"(?i)(?:flashvars\s*=\s*|name\s*=\s*["']flashvars["']\s+value\s*=\s*)["']([^"']*)["']"#,
        )
        .unwrap()
    });
    let raw = pattern.captures(body)?.get(1)?.as_str();
    let mut vars = HashMap::new();
    for part in raw.split('&') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if !key.is_empty() {
            vars.insert(key.to_string(), value.to_string());
        }
    }
    vars.contains_key("sessionID").then_some(vars)
}

fn resolve_movie(instance: &str, movie: &str) -> String {
    if movie.starts_with("http://") || movie.starts_with("https://") {
        return movie.to_string();
    }
    reqwest::Url::parse(instance)
        .and_then(|base| base.join(movie))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| movie.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_omits_authenticated_flashvars() {
        let page = r#"flashembed("c", {"src":"https://cdn.example/spacemap/preloader.swf","width":"100%","height":"100%"}, {"cdn":"https://cdn.example/","mapID":"1","display2d":"2"});"#;
        let embed = parse_embed(page).unwrap();
        assert_eq!(embed.src, "https://cdn.example/spacemap/preloader.swf");
        assert_eq!(embed.width, 100);
        assert_eq!(embed.height, 100);
        assert_eq!(embed.flashvars.get("mapID").map(String::as_str), Some("1"));
        assert!(!embed.flashvars.contains_key("userID"));
        assert!(!embed.flashvars.contains_key("sessionID"));
        assert!(!embed.flashvars.contains_key("factionID"));
    }

    #[test]
    fn script_embed_is_javascript_and_already_authenticated() {
        let page = r#"flashembed("container", {"onFail": onFailFlashembed, "src": "https://cdn.example/spacemap/preloader.swf","version": [11,0],"width": "100%","height": "100%"}, {"lang": "tr","userID": "42","sessionID": "abc","cdn": "https://cdn.example/","mapID": "1","display2d": "2"});"#;
        let embed = parse_embed(page).unwrap();
        assert_eq!(embed.src, "https://cdn.example/spacemap/preloader.swf");
        assert_eq!(embed.width, 100);
        assert_eq!(embed.height, 100);
        assert_eq!(
            embed.flashvars.get("userID").map(String::as_str),
            Some("42")
        );
        assert_eq!(
            embed.flashvars.get("sessionID").map(String::as_str),
            Some("abc")
        );
        assert_eq!(embed.flashvars.get("lang").map(String::as_str), Some("tr"));
    }

    #[test]
    fn loading_screen_supplies_authenticated_contract() {
        let json = r#"{"userID":"42","sessionID":"abc","factionID":"1","lang":"tr"}"#;
        let vars = parse_authenticated_flashvars(json).unwrap();
        assert_eq!(vars.get("userID").map(String::as_str), Some("42"));
        assert_eq!(vars.get("sessionID").map(String::as_str), Some("abc"));
        assert_eq!(vars.get("factionID").map(String::as_str), Some("1"));

        let html = r#"<param name="flashvars" value="userID=7&sessionID=from-object&factionID=3&mapID=1" />"#;
        let from_html = parse_authenticated_flashvars(html).unwrap();
        assert_eq!(
            from_html.get("sessionID").map(String::as_str),
            Some("from-object")
        );
        assert_eq!(from_html.get("factionID").map(String::as_str), Some("3"));
    }
}
