use crate::client::{BackpageClient, Param};
use crate::error::{Error, Result};
use serde::Deserialize;
use serde_json::Value;

const CAPTAIN_PATH: &str = "ajax/captain.php";

/// `nova/Agent.java` — a P.E.T. captain.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct Agent {
    #[serde(rename = "captainId", default)]
    pub captain_id: i64,
    #[serde(rename = "captainTypeId", default)]
    pub captain_type_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub energy: i64,
    #[serde(rename = "energyMax", default)]
    pub energy_max: i64,
    #[serde(rename = "energyFullTime", default)]
    pub energy_full_time: i64,
    #[serde(default)]
    pub perks: Vec<Perk>,
    #[serde(default)]
    pub equipped: Option<bool>,
    #[serde(rename = "dismissCost", default)]
    pub dismiss_cost: Option<Value>,
    #[serde(rename = "regenAmount", default)]
    pub regen_amount: i64,
    #[serde(rename = "regenRate", default)]
    pub regen_rate: f64,
    #[serde(default)]
    pub desc: Option<String>,
}

/// `nova/Perk.java`.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct Perk {
    #[serde(rename = "perkId", default)]
    pub perk_id: i64,
    #[serde(rename = "perkTypeId", default)]
    pub perk_type_id: Option<String>,
    #[serde(default)]
    pub level: i64,
    #[serde(rename = "levelMax", default)]
    pub level_max: i64,
    #[serde(default)]
    pub drain: i64,
    #[serde(default)]
    pub modifier: f64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(rename = "drainDesc", default)]
    pub drain_desc: Option<String>,
    #[serde(default)]
    pub upgrades: Vec<Value>,
}

fn check_result(json: &Value) -> Result<()> {
    match json.get("result").and_then(Value::as_str) {
        Some(r) if r.eq_ignore_ascii_case("OK") => Ok(()),
        other => Err(Error::Api(format!(
            "unexpected result {other:?}: {}",
            &json.to_string()[..json.to_string().len().min(200)]
        ))),
    }
}

impl BackpageClient {
    async fn captain_cmd(&self, params: &[Param]) -> Result<Value> {
        let json = self.ajax(CAPTAIN_PATH, params).await?;
        check_result(&json)?;
        Ok(json)
    }

    /// `updateActiveCaptain` — pass 0 to just read the current one.
    pub async fn captain_set_active(&self, captain_id: i64) -> Result<i64> {
        let json = self
            .captain_cmd(&[
                Param::new("command", "updateActiveCaptain"),
                Param::new(
                    "captainId",
                    if captain_id == 0 {
                        String::new()
                    } else {
                        captain_id.to_string()
                    },
                ),
            ])
            .await?;
        Ok(json
            .get("activeCaptainId")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0))
    }

    /// `getResources` — nova item amount.
    pub async fn captain_resources(&self) -> Result<i64> {
        let json = self
            .captain_cmd(&[Param::new("command", "getResources")])
            .await?;
        Ok(json
            .pointer("/item/amount")
            .and_then(Value::as_i64)
            .unwrap_or(0))
    }

    /// `getRosterList` — all owned captains.
    pub async fn captain_roster(&self) -> Result<Vec<Agent>> {
        let json = self
            .captain_cmd(&[Param::new("command", "getRosterList")])
            .await?;
        Ok(serde_json::from_value(
            json.get("rosterList").cloned().unwrap_or(Value::Null),
        )?)
    }

    pub async fn captain_dismiss(&self, captain_id: i64) -> Result<()> {
        self.captain_cmd(&[
            Param::new("command", "dismissCaptain"),
            Param::new("captainId", captain_id.to_string()),
        ])
        .await?;
        Ok(())
    }

    /// `getPerkUpgradeDetail`.
    pub async fn captain_perk_detail(&self, captain_id: i64, perk_id: i64) -> Result<Perk> {
        let json = self
            .captain_cmd(&[
                Param::new("command", "getPerkUpgradeDetail"),
                Param::new("captainId", captain_id.to_string()),
                Param::new("perkId", perk_id.to_string()),
            ])
            .await?;
        Ok(serde_json::from_value(
            json.get("perk").cloned().unwrap_or(Value::Null),
        )?)
    }

    /// `upgradePerk`.
    pub async fn captain_upgrade_perk(
        &self,
        captain_id: i64,
        perk_id: i64,
        upgrade_level: i64,
    ) -> Result<()> {
        self.captain_cmd(&[
            Param::new("command", "upgradePerk"),
            Param::new("captainId", captain_id.to_string()),
            Param::new("perkId", perk_id.to_string()),
            Param::new("upgradeLevel", upgrade_level.to_string()),
        ])
        .await?;
        Ok(())
    }

    /// `NovaManager.buyNova` — buys novas via the shop endpoint.
    pub async fn captain_buy_nova(&self, amount: u64) -> Result<()> {
        self.shop_purchase("special", "captain_captain-generic", amount)
            .await?;
        Ok(())
    }
}
