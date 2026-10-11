use crate::base62;
use crate::client::{BackpageClient, Param, url_decode};
use crate::error::{Error, Result};
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

fn profile_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new("/p/([A-Za-z0-9]+)-").unwrap())
}

/// `UsernameUpdater.UserResponse`.
#[derive(Debug, Deserialize)]
pub struct UserResponse {
    #[serde(default)]
    pub result: String,
    #[serde(rename = "userName", default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

impl UserResponse {
    pub fn ok(&self) -> bool {
        self.result.eq_ignore_ascii_case("OK")
    }

    /// `userName` arrives URL-encoded.
    pub fn username(&self) -> Option<String> {
        self.user_name.as_deref().map(url_decode)
    }

    /// Profile id embedded in `url` as `/p/<base62>-...`.
    pub fn profile_id(&self) -> Option<i64> {
        let url = self.url.as_deref()?;
        let caps = profile_id_re().captures(url)?;
        match base62::decode(&caps[1]) {
            0 => None,
            id => Some(id),
        }
    }
}

#[derive(Debug)]
pub struct ResolvedUser {
    pub user_id: i64,
    pub username: Option<String>,
}

impl BackpageClient {
    /// `ajax/user.php` `loadUserInfo` — id (base62) → username.
    pub async fn user_by_id(&self, user_id: i64) -> Result<ResolvedUser> {
        let json = self
            .ajax(
                "ajax/user.php",
                &[
                    Param::new("command", "loadUserInfo"),
                    Param::new("userId", base62::encode(user_id)),
                ],
            )
            .await?;
        let resp: UserResponse = serde_json::from_value(json)?;
        if !resp.ok() {
            return Err(Error::Api(format!("loadUserInfo: {}", resp.result)));
        }
        Ok(ResolvedUser {
            user_id,
            username: resp.username(),
        })
    }

    /// `ajax/pilotprofil.php` `searchProfileFromExternalPPP` — username → id.
    pub async fn user_by_name(&self, username: &str) -> Result<ResolvedUser> {
        let json = self
            .ajax(
                "ajax/pilotprofil.php",
                &[
                    Param::new("command", "searchProfileFromExternalPPP"),
                    Param::new("profileUsername", username),
                ],
            )
            .await?;
        let resp: UserResponse = serde_json::from_value(json)?;
        if !resp.ok() {
            return Err(Error::Api(format!("searchProfile: {}", resp.result)));
        }
        let id = resp
            .profile_id()
            .ok_or_else(|| Error::Api("player not found".into()))?;
        Ok(ResolvedUser {
            user_id: id,
            username: resp.username().or_else(|| Some(username.to_string())),
        })
    }
}
