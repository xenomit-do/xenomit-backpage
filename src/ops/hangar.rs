use crate::client::{BackpageClient, Param, url_encode};
use crate::error::{Error, Result};
use base64::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;

/// `flashAPI/inventory.php` responses: base64-encoded JSON.
#[derive(Debug, Deserialize, Serialize)]
pub struct HangarResponse {
    #[serde(rename = "isError")]
    pub is_error: i32,
    #[serde(default)]
    pub data: Option<HangarData>,
    /// Anything else the endpoint returns.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HangarData {
    #[serde(default)]
    pub ret: Option<Ret>,
    #[serde(default)]
    pub money: Option<Money>,
    /// String lookup tables (`lootIds`, `types`, ...); cleared after resolving.
    #[serde(default)]
    pub map: Option<HashMap<String, Vec<String>>>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Money {
    #[serde(default)]
    pub uridium: Option<String>,
    #[serde(default)]
    pub credits: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Ret {
    /// `hangars` arrives as either a JSON array or an object keyed by id
    /// (`Hangar.HangarAdapter` tolerates both).
    #[serde(default, deserialize_with = "deserialize_hangars")]
    pub hangars: Vec<Hangar>,
    #[serde(rename = "items", default)]
    pub items: Vec<Value>,
    #[serde(rename = "itemInfo", default)]
    pub item_infos: Vec<Value>,
    #[serde(rename = "shipInfo", default)]
    pub ship_infos: Vec<Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn deserialize_string_or_i64<'de, D>(d: D) -> std::result::Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| Error::custom("hangarID number out of range")),
        Value::String(s) => s.parse().map_err(Error::custom),
        other => Err(Error::custom(format!(
            "hangarID expected string or number, got {other}"
        ))),
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Hangar {
    #[serde(rename = "hangarID", deserialize_with = "deserialize_string_or_i64")]
    pub hangar_id: i64,
    #[serde(rename = "hangar_is_active", default)]
    pub active: bool,
    #[serde(default)]
    pub general: Option<Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Untagged adapter for the array-or-object `hangars` field.
#[derive(Deserialize)]
#[serde(untagged)]
enum HangarsRaw {
    List(Vec<Hangar>),
    Map(HashMap<String, Hangar>),
}

fn deserialize_hangars<'de, D>(d: D) -> std::result::Result<Vec<Hangar>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<HangarsRaw>::deserialize(d)? {
        Some(HangarsRaw::List(v)) => Ok(v),
        Some(HangarsRaw::Map(m)) => Ok(m.into_values().collect()),
        None => Ok(vec![]),
    }
}

/// One drone row from `getHangar` → `general.drones`, mirroring `entities/Drone.java`.
#[derive(Debug, Clone, Serialize)]
pub struct Drone {
    pub slot: u32,
    pub item_id: String,
    pub loot_id: String,
    pub level: u32,
    pub damage_percent: u32,
    pub repair_price: u32,
    pub repair_currency: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DroneRepairResult {
    pub slot: u32,
    pub item_id: String,
    pub ok: bool,
}

fn decode_inventory_response(body: &str) -> Result<HangarResponse> {
    let decoded = BASE64_STANDARD
        .decode(body.trim())
        .map_err(|e| Error::Parse(format!("inventory base64: {e}: {body}")))?;
    serde_json::from_slice(&decoded).map_err(|e| Error::Parse(format!("inventory json: {e}")))
}

fn resolve_loot_ids(hangar: &mut HangarResponse) {
    if let Some(data) = hangar.data.as_mut()
        && let (Some(ret), Some(map)) = (data.ret.as_mut(), data.map.take())
        && let Some(loot_ids) = map.get("lootIds")
    {
        for info in &mut ret.item_infos {
            if let Some(idx) = info.get("lootId").and_then(Value::as_u64)
                && let Some(loc) = loot_ids.get(idx as usize)
            {
                info["localizationId"] = json!(loc);
            }
        }
    }
}

fn loot_id_from_index(index: u64) -> &'static str {
    match index {
        1 => "drone_flax",
        2 => "drone_iris",
        3 => "drone_apis",
        4 => "drone_zeus",
        _ => "drone_iris",
    }
}

/// Parse `"45%"` / `"100"` style HP fields into 0–100 damage percent.
pub fn parse_drone_damage(hp: &str) -> u32 {
    hp.chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
        .min(100)
}

fn drone_from_value(slot: u32, value: &Value) -> Option<Drone> {
    let item_id = match value.get("I") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return None,
    };
    let loot_id = match value.get("L") {
        Some(Value::Number(n)) => loot_id_from_index(n.as_u64().unwrap_or(2)).to_string(),
        Some(Value::String(s)) if s.starts_with("drone_") => s.clone(),
        Some(Value::String(s)) => s
            .parse::<u64>()
            .ok()
            .map(|i| loot_id_from_index(i).to_string())?,
        _ => "drone_iris".into(),
    };
    Some(Drone {
        slot,
        item_id,
        loot_id,
        level: value.get("LV").and_then(Value::as_u64).unwrap_or(1) as u32,
        damage_percent: value
            .get("HP")
            .and_then(Value::as_str)
            .map(parse_drone_damage)
            .unwrap_or(0),
        repair_price: value.get("repair").and_then(Value::as_u64).unwrap_or(0) as u32,
        repair_currency: value
            .get("currency")
            .and_then(Value::as_str)
            .unwrap_or("credits")
            .to_string(),
    })
}

/// Drones from the active hangar in a `getHangar` response.
pub fn drones_from_hangar(hangar: &HangarResponse) -> Vec<Drone> {
    let Some(ret) = hangar.data.as_ref().and_then(|d| d.ret.as_ref()) else {
        return vec![];
    };
    let active = ret
        .hangars
        .iter()
        .find(|h| h.active)
        .or_else(|| ret.hangars.first());
    let Some(active) = active else {
        return vec![];
    };
    let Some(drones) = active
        .general
        .as_ref()
        .and_then(|g| g.get("drones"))
        .and_then(|d| d.as_array())
    else {
        return vec![];
    };
    drones
        .iter()
        .enumerate()
        .filter_map(|(idx, d)| drone_from_value(idx as u32 + 1, d))
        .collect()
}

impl BackpageClient {
    /// Shared `flashAPI/inventory.php` POST: `action` raw, `params` = base64(json).
    async fn inventory(&self, action: &str, params: Value) -> Result<HangarResponse> {
        let encoded = BASE64_STANDARD.encode(params.to_string());
        let resp = self
            .post(
                "flashAPI/inventory.php",
                &[Param::raw("action", action), Param::new("params", &encoded)],
            )
            .await?;
        let mut hangar = decode_inventory_response(&resp.body)?;
        resolve_loot_ids(&mut hangar);
        Ok(hangar)
    }

    /// `LegacyHangarManager.repairDrone` — GET with nested params in the query string.
    async fn inventory_get(&self, action: &str, params: Value) -> Result<HangarResponse> {
        let encoded = BASE64_STANDARD.encode(params.to_string());
        let path = format!(
            "flashAPI/inventory.php?action={}&params={}",
            url_encode(action),
            url_encode(&encoded)
        );
        let resp = self.get(&path, &[]).await?;
        let mut hangar = decode_inventory_response(&resp.body)?;
        resolve_loot_ids(&mut hangar);
        Ok(hangar)
    }

    pub async fn hangar_list(&self) -> Result<HangarResponse> {
        self.inventory("getHangarList", json!({})).await
    }

    pub async fn hangar(&self, hangar_id: i64) -> Result<HangarResponse> {
        self.inventory("getHangar", json!({ "params": { "hi": hangar_id } }))
            .await
    }

    /// Active hangar id from the list, 0 when unknown (`getCurrentHangarId`).
    pub async fn active_hangar_id(&self) -> Result<i64> {
        let list = self.hangar_list().await?;
        Ok(list
            .data
            .and_then(|d| d.ret)
            .and_then(|r| r.hangars.into_iter().find(|h| h.active))
            .map(|h| h.hangar_id)
            .unwrap_or(0))
    }

    /// Active hangar, fetching the list first when needed (`updateCurrentHangar`).
    pub async fn current_hangar(&self) -> Result<HangarResponse> {
        let id = self.active_hangar_id().await?;
        if id == 0 {
            return Err(Error::Api("no active hangar".into()));
        }
        self.hangar(id).await
    }

    /// `LegacyHangarManager.changeHangar` — `activateShip` on `inventory.php`.
    pub async fn activate_ship(&self, hangar_id: i64) -> Result<bool> {
        let active = self.active_hangar_id().await?;
        let params = json!({ "params": { "hi": active, "hangarId": hangar_id.to_string() } });
        let resp = self.inventory("activateShip", params).await?;
        Ok(resp.is_error == 0)
    }

    /// Repair one drone via `flashAPI/inventory.php?action=repairDrone`.
    pub async fn repair_drone(&self, hangar_id: i64, drone: &Drone) -> Result<bool> {
        let params = json!({
            "action": "repairDrone",
            "lootId": drone.loot_id,
            "repairPrice": drone.repair_price,
            "params": { "hi": hangar_id },
            "itemId": drone.item_id,
            "repairCurrency": drone.repair_currency,
            "quantity": 1,
            "droneLevel": drone.level,
        });
        let resp = self.inventory_get("repairDrone", params).await?;
        Ok(resp.is_error == 0)
    }

    /// Repair drones at or above `damage_threshold` percent (0–100).
    /// Uses the same 12 s cooldown between repairs as `LegacyHangarManager`.
    pub async fn repair_drones(
        &self,
        damage_threshold: u32,
        slot: Option<u32>,
    ) -> Result<Vec<DroneRepairResult>> {
        let hangar_id = self.active_hangar_id().await?;
        if hangar_id == 0 {
            return Err(Error::Api("no active hangar".into()));
        }
        let hangar = self.hangar(hangar_id).await?;
        let mut results = Vec::new();
        for drone in drones_from_hangar(&hangar) {
            if slot.is_some_and(|s| s != drone.slot) {
                continue;
            }
            if drone.damage_percent < damage_threshold {
                continue;
            }
            let ok = self.repair_drone(hangar_id, &drone).await?;
            results.push(DroneRepairResult {
                slot: drone.slot,
                item_id: drone.item_id.clone(),
                ok,
            });
            tokio::time::sleep(Duration::from_secs(12)).await;
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_drone_damage() {
        assert_eq!(parse_drone_damage("45%"), 45);
        assert_eq!(parse_drone_damage("100"), 100);
        assert_eq!(parse_drone_damage("?"), 0);
    }

    #[test]
    fn deserializes_string_hangar_ids() {
        let json = r#"{
            "hangarID": "2455647",
            "hangar_is_active": true,
            "general": {}
        }"#;
        let hangar: Hangar = serde_json::from_str(json).expect("hangar should parse");
        assert_eq!(hangar.hangar_id, 2_455_647);
        assert!(hangar.active);
    }
}
