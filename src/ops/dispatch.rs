use crate::client::BackpageClient;
use crate::error::{Error, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize)]
pub struct DispatchRetriever {
    pub id: String,
    pub name: String,
    pub r#type: String,
    pub tier: String,
    pub cost: String,
    pub credit_cost: u32,
    pub uridium_cost: u32,
    pub permit_cost: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchInProgress {
    pub slot_id: String,
    pub id: String,
    pub name: String,
    pub collectable: String,
    pub reward_package: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchGate {
    pub id: String,
    pub name: String,
    pub time_left: String,
    pub cost: String,
    pub collectable: String,
    pub in_progress: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DispatchData {
    pub retrievers: BTreeMap<String, DispatchRetriever>,
    pub in_progress: BTreeMap<String, DispatchInProgress>,
    pub gates: BTreeMap<String, DispatchGate>,
}

fn retriever_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)",
            r#"dispatchId="(.+?)".*?"#,
            r#"dispatch_item_name_col"> ?(.+?) ?<.*?"#,
            r#"dispatch_item_type"> ?(.+?) ?<.*?"#,
            r#"dispatch_item_tier"> ?(.+?) ?<.*?"#,
            r#"dispatch_item_cost"> ?(.+?) ?<br>"#,
        ))
        .unwrap()
    })
}

fn retriever_dual_cost_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)",
            r#"dispatchId="(.+?)".*?"#,
            r#"dispatch_item_name_col"> ?(.+?) ?<.*?"#,
            r#"dispatch_item_type"> ?(.+?) ?<.*?"#,
            r#"dispatch_item_tier"> ?(.+?) ?<.*?"#,
            r#"dispatch_item_cost"> ?(.+?) ?<br> ?(.+?) ?<br>"#,
        ))
        .unwrap()
    })
}

fn progress_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)",
            r#"collectable="(\d+)".*?"#,
            r#"dispatchId="(.+?)".*?"#,
            r#"dispatchRewardPackage="(.+?)".*?"#,
            r#"slotId="(\d+)".*?"#,
            r#"dispatch_item_name_col"> ?(.+?) ?<"#,
        ))
        .unwrap()
    })
}

fn gate_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)",
            r#"gateId="([^"]+)"(?:\scollectable="(\d+)")?.*?"#,
            r#"dispatch_item_name_col"> ?(.+?) ?<.*?"#,
            r#"dispatch_remaining_time_col"> ?(.+?) ?<input.*?"#,
            r#"dispatch_item_cost">(.+?)?<"#,
        ))
        .unwrap()
    })
}

fn cost_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d+)").unwrap())
}

fn first_cost(s: &str) -> u32 {
    cost_re()
        .captures(s)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0)
}

fn normalize_row(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_retriever_row(row: &str) -> Option<DispatchRetriever> {
    if !row.contains("dispatch_item_name_col")
        || !row.contains("dispatch_item_type")
        || !row.contains("dispatch_item_tier")
        || !row.contains("dispatch_item_cost")
    {
        return None;
    }
    let normalized = normalize_row(row);
    if let Some(caps) = retriever_dual_cost_re().captures(&normalized) {
        return Some(DispatchRetriever {
            id: caps[1].to_string(),
            name: caps[2].trim().to_string(),
            r#type: caps[3].trim().to_string(),
            tier: caps[4].trim().to_string(),
            cost: format!("{} & {}", caps[5].trim(), caps[6].trim()),
            credit_cost: 0,
            uridium_cost: first_cost(caps[5].trim()),
            permit_cost: first_cost(caps[6].trim()),
        });
    }
    let caps = retriever_re().captures(&normalized)?;
    Some(DispatchRetriever {
        id: caps[1].to_string(),
        name: caps[2].trim().to_string(),
        r#type: caps[3].trim().to_string(),
        tier: caps[4].trim().to_string(),
        cost: caps[5].trim().to_string(),
        credit_cost: first_cost(caps[5].trim()),
        uridium_cost: 0,
        permit_cost: 0,
    })
}

fn parse_progress_row(row: &str) -> Option<DispatchInProgress> {
    if !row.contains("dispatchRewardPackage")
        || !row.contains("slotId")
        || !row.contains("dispatch_item_name_col")
    {
        return None;
    }
    let normalized = normalize_row(row);
    let caps = progress_re().captures(&normalized)?;
    Some(DispatchInProgress {
        collectable: caps[1].to_string(),
        id: caps[2].to_string(),
        reward_package: caps[3].to_string(),
        slot_id: caps[4].to_string(),
        name: caps[5].trim().to_string(),
    })
}

fn parse_gate_row(row: &str) -> Option<DispatchGate> {
    if !row.contains("gateId") {
        return None;
    }
    let normalized = normalize_row(row);
    let caps = gate_re().captures(&normalized)?;
    let collectable = caps.get(2).map(|m| m.as_str()).unwrap_or("0").to_string();
    let time_left = {
        let t = caps[4].trim();
        if t.len() < 10 {
            t.to_string()
        } else {
            "0".into()
        }
    };
    let cost = {
        let c = caps[5].trim();
        if c.len() > 5 {
            c.to_string()
        } else {
            "0".into()
        }
    };
    let in_progress = collectable == "1" || (time_left == "0" && cost == "0");
    Some(DispatchGate {
        id: caps[1].to_string(),
        name: caps[3].trim().to_string(),
        time_left,
        cost,
        collectable,
        in_progress,
    })
}

/// Port of `dispatch/DispatchData.parseRetrieverRow`.
pub fn parse_dispatch_page(page: &str) -> DispatchData {
    let mut data = DispatchData::default();
    for row in page.split("</tr>") {
        if let Some(r) = parse_retriever_row(row) {
            data.retrievers.insert(r.id.clone(), r);
        } else if let Some(p) = parse_progress_row(row) {
            data.in_progress.insert(p.slot_id.clone(), p);
        } else if let Some(g) = parse_gate_row(row) {
            data.gates.insert(g.id.clone(), g);
        }
    }
    data
}

impl BackpageClient {
    /// Scrape `internalDispatch` into retriever / in-progress / gate rows.
    pub async fn dispatch_data(&self) -> Result<DispatchData> {
        let resp = self
            .get("indexInternal.es?action=internalDispatch", &[])
            .await?;
        if resp.body.contains("redirectToLogin") || resp.status == reqwest::StatusCode::FOUND {
            return Err(Error::SessionExpired);
        }
        Ok(parse_dispatch_page(&resp.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_retriever_row() {
        let row = r#"<tr dispatchId="abc123" class="x">
            <td class="dispatch_item_name_col"> Alpha Hunter </td>
            <td class="dispatch_item_type"> Credits </td>
            <td class="dispatch_item_tier"> Tier 1 </td>
            <td class="dispatch_item_cost"> 500.000 <br> </td>
        </tr>"#;
        let r = parse_retriever_row(row).expect("retriever");
        assert_eq!(r.id, "abc123");
        assert_eq!(r.name, "Alpha Hunter");
        assert_eq!(r.credit_cost, 500);
    }

    #[test]
    fn parses_gate_row() {
        let row = r#"<tr gateId="gate1" collectable="1">
            <td class="dispatch_item_name_col"> Alpha Gate </td>
            <td class="dispatch_remaining_time_col"> 01:30 <input /></td>
            <td class="dispatch_item_cost">100 U.</td>
        </tr>"#;
        let g = parse_gate_row(row).expect("gate");
        assert_eq!(g.id, "gate1");
        assert_eq!(g.name, "Alpha Gate");
        assert!(g.in_progress);
    }
}
