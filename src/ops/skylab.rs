use crate::client::BackpageClient;
use crate::error::{Error, Result};
use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize)]
pub struct SkylabModule {
    pub id: String,
    pub name: String,
    pub level: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkylabStatus {
    pub modules: Vec<SkylabModule>,
    /// True when the page loaded but no module rows were found (client-side UI only).
    pub parse_limited: bool,
}

fn module_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)",
            r#"(?:moduleId|buildingId|skylabModuleId)="([^"]+)".*?"#,
            r#"(?:module_name|building_name|skylab_item_name)[^>]*>\s*([^<]+?)\s*<.*?"#,
            r#"(?:module_level|building_level|level)[^>]*>\s*(\d+)\s*<"#,
        ))
        .unwrap()
    })
}

fn fallback_module_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)skylab[_-]([a-z0-9]+)[^>]*>[^<]*<[^>]*>\s*(\d+)\s*<"#).unwrap()
    })
}

/// Best-effort scrape of `internalSkylab` HTML. DarkBot only pings this page for SID
/// health; transport/refine still require the in-game client.
pub fn parse_skylab_page(page: &str) -> SkylabStatus {
    let mut modules = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for caps in module_re().captures_iter(page) {
        let id = caps[1].to_string();
        if !seen.insert(id.clone()) {
            continue;
        }
        modules.push(SkylabModule {
            id,
            name: caps[2].trim().to_string(),
            level: caps[3].parse().unwrap_or(0),
        });
    }

    if modules.is_empty() {
        for caps in fallback_module_re().captures_iter(page) {
            let id = caps[1].to_string();
            if !seen.insert(id.clone()) {
                continue;
            }
            modules.push(SkylabModule {
                id: id.clone(),
                name: id.replace('_', " "),
                level: caps[2].parse().unwrap_or(0),
            });
        }
    }

    SkylabStatus {
        parse_limited: modules.is_empty(),
        modules,
    }
}

impl BackpageClient {
    /// Raw `internalSkylab` HTML used by the builder task.
    pub async fn skylab_page(&self) -> Result<String> {
        let resp = self
            .get("indexInternal.es?action=internalSkylab", &[])
            .await?;
        if resp.body.contains("redirectToLogin") || resp.status == reqwest::StatusCode::FOUND {
            return Err(Error::SessionExpired);
        }
        Ok(resp.body)
    }

    /// `command=buySkylabRobot`. `uri` selects Uridium robots (type 2).
    pub async fn buy_skylab_robot(&self, construction: &str, uri: bool) -> Result<()> {
        let resp = self
            .post(
                "ajax/skylab.php",
                &[
                    crate::Param::new("command", "buySkylabRobot"),
                    crate::Param::new("type", if uri { "2" } else { "1" }),
                    crate::Param::new("construction", construction),
                ],
            )
            .await?;
        if resp.body.contains("redirectToLogin") || resp.status == reqwest::StatusCode::FOUND {
            return Err(Error::SessionExpired);
        }
        Ok(())
    }

    pub async fn skylab_status(&self) -> Result<SkylabStatus> {
        let resp = self
            .get("indexInternal.es?action=internalSkylab", &[])
            .await?;
        if resp.body.contains("redirectToLogin") || resp.status == reqwest::StatusCode::FOUND {
            return Err(Error::SessionExpired);
        }
        Ok(parse_skylab_page(&resp.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_module_row() {
        let page = r#"
            <div moduleId="basic" class="skylab">
              <span class="module_name">Basic module</span>
              <span class="module_level">12</span>
            </div>
        "#;
        let status = parse_skylab_page(page);
        assert_eq!(status.modules.len(), 1);
        assert_eq!(status.modules[0].level, 12);
    }
}
