use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Session represents a `dosid` session bound to a DarkOrbit game instance.
///
/// It is used to authenticate and interact with the backpage API.
/// It also includes flashvars scraped from the preloader page, required for authentication.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Session {
    pub sid: String,
    /// Host of the game instance, e.g. `int1.darkorbit.com`.
    pub server: String,
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub preloader_url: Option<String>,
    /// Pixel size from the embed. Values at or below 100 are CSS percentages.
    #[serde(default)]
    pub stage_width: i32,
    #[serde(default)]
    pub stage_height: i32,
    #[serde(default)]
    pub flashvars: HashMap<String, String>,
    #[serde(default)]
    pub authenticated_at: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Explicit base URL overriding `https://<server>/` in [`Session::instance`].
    /// Test/mock-server seam; `None` in production sessions.
    #[serde(default)]
    pub instance_base: Option<String>,
}

impl Session {
    pub fn new(sid: impl Into<String>, server: impl Into<String>) -> Self {
        let mut server = server.into();
        // Accept both "int1" and "int1.darkorbit.com" / full URLs.
        if let Some(rest) = server.strip_prefix("https://") {
            server = rest.to_string();
        } else if let Some(rest) = server.strip_prefix("http://") {
            server = rest.to_string();
        }
        server = server.trim_end_matches('/').to_string();
        if !server.contains('.') {
            server = format!("{server}.darkorbit.com");
        }
        Session {
            sid: sid.into(),
            server,
            ..Default::default()
        }
    }

    /// `https://<server>/` — every backpage path is appended to this.
    /// [`Session::instance_base`] takes precedence when set.
    pub fn instance(&self) -> String {
        match &self.instance_base {
            Some(base) => format!("{}/", base.trim_end_matches('/')),
            None => format!("https://{}/", self.server),
        }
    }

    /// Point [`Session::instance`] at an explicit base URL — used by headless
    /// daemons and tests against mock Backpage servers. `None` restores the
    /// server-derived URL.
    pub fn set_instance_base(&mut self, base: Option<String>) {
        self.instance_base = base
            .map(|base| base.trim().trim_end_matches('/').to_string())
            .filter(|base| !base.is_empty());
    }

    pub fn is_valid(&self) -> bool {
        !self.sid.is_empty() && !self.server.is_empty() && self.user_id != 0
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        use std::io::Write;

        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    }

    pub fn start_ttl(&mut self, ttl_secs: u64) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Api("system clock is before Unix epoch".into()))?
            .as_secs();
        self.authenticated_at = Some(now);
        self.expires_at = Some(
            now.checked_add(ttl_secs)
                .ok_or_else(|| Error::Api("session TTL is too large".into()))?,
        );
        Ok(())
    }

    pub fn is_expired_at(&self, now: u64) -> bool {
        match (self.authenticated_at, self.expires_at) {
            (Some(start), Some(end)) => now < start || now >= end || end <= start,
            _ => true,
        }
    }

    pub fn is_expired(&self) -> bool {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|now| self.is_expired_at(now.as_secs()))
            .unwrap_or(true)
    }

    /// Extract `userID` from flashvars, mirroring `LoginData.setPreloader`.
    pub fn set_preloader(&mut self, preloader_url: String, vars: HashMap<String, String>) {
        self.preloader_url = Some(preloader_url);
        self.user_id = vars.get("userID").and_then(|v| v.parse().ok()).unwrap_or(0);
        self.flashvars = vars;
    }

    pub fn set_stage_size(&mut self, width: i32, height: i32) {
        self.stage_width = width;
        self.stage_height = height;
    }

    pub fn require_valid(&self) -> Result<()> {
        if self.sid.is_empty() || self.server.is_empty() {
            return Err(Error::InvalidSession);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL_30M: u64 = 30 * 60;

    #[test]
    fn not_expired_before_ttl() {
        let mut s = Session::new("abc", "int1");
        s.start_ttl(TTL_30M).unwrap();
        assert!(!s.is_expired_at(s.authenticated_at.unwrap()));
        assert!(!s.is_expired_at(s.expires_at.unwrap() - 1));
    }

    #[test]
    fn expired_at_ttl_boundary_and_after() {
        let mut s = Session::new("abc", "int1");
        s.start_ttl(TTL_30M).unwrap();
        let end = s.expires_at.unwrap();
        assert!(s.is_expired_at(end));
        assert!(s.is_expired_at(end + 1));
    }

    #[test]
    fn legacy_session_without_timestamps_is_expired() {
        let json = r#"{"sid":"abc","server":"int1.darkorbit.com"}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert!(s.is_expired());
    }

    #[test]
    fn invalid_timestamps_are_expired() {
        let mut s = Session::new("abc", "int1");
        s.authenticated_at = Some(100);
        s.expires_at = Some(100);
        assert!(s.is_expired_at(100));
        assert!(s.is_expired_at(50));
        s.authenticated_at = Some(200);
        s.expires_at = Some(100);
        assert!(s.is_expired_at(300));
    }

    #[test]
    fn save_load_retains_expiry_without_sliding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let mut s = Session::new("abc", "int1");
        s.start_ttl(TTL_30M).unwrap();
        let start = s.authenticated_at.unwrap();
        let end = s.expires_at.unwrap();
        s.save(&path).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.authenticated_at, Some(start));
        assert_eq!(loaded.expires_at, Some(end));
        assert_eq!(loaded.sid, "abc");
    }

    #[test]
    fn saved_session_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, "wide-open").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let mut s = Session::new("abc", "int1");
        s.start_ttl(TTL_30M).unwrap();
        s.save(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
