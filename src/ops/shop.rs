use crate::client::{BackpageClient, Param};
use crate::error::{Error, Result};

impl BackpageClient {
    /// `ajax/shop.php` `action=purchase` — e.g. `battery`/`ammunition_laser_lcb-10`.
    /// Returns the server's `result` string ("success" on success).
    pub async fn shop_purchase(
        &self,
        category: &str,
        item_id: &str,
        amount: u64,
    ) -> Result<String> {
        let resp = self
            .post(
                "ajax/shop.php",
                &[
                    Param::new("action", "purchase"),
                    Param::new("category", category),
                    Param::new("itemId", item_id),
                    Param::new("amount", amount.to_string()),
                    Param::new("level", ""),
                    Param::new("selectedName", ""),
                ],
            )
            .await?;
        if resp.status == reqwest::StatusCode::FOUND || resp.body.contains("redirectToLogin") {
            return Err(Error::SessionExpired);
        }
        let json: serde_json::Value = serde_json::from_str(&resp.body)
            .map_err(|e| Error::Parse(format!("{e}: {}", crate::client::truncate(&resp.body))))?;
        match json.get("result").and_then(|r| r.as_str()) {
            Some("success") => Ok("success".into()),
            other => Err(Error::Api(format!(
                "purchase result {other:?}: {}",
                crate::client::truncate(&resp.body)
            ))),
        }
    }
}
