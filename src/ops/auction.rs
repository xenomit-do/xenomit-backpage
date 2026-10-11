use crate::base62;
use crate::client::{BackpageClient, Param};
use crate::error::{Error, Result};
use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuctionType {
    Hour,
    Day,
    Week,
}

impl AuctionType {
    pub fn id(&self) -> &'static str {
        match self {
            AuctionType::Hour => "hour",
            AuctionType::Day => "day",
            AuctionType::Week => "week",
        }
    }
}

/// One row of the auction house, scraped like `auction/AuctionData.java`.
#[derive(Debug, Clone, Serialize)]
pub struct AuctionItem {
    /// e.g. `item_hour_123`
    pub id: String,
    pub loot_id: String,
    pub name: String,
    pub item_type: String,
    #[serde(rename = "type")]
    pub auction_type: AuctionType,
    pub highest_bidder_id: i64,
    pub current_bid: u64,
    pub own_bid: u64,
    pub instant_buy: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuctionSnipeResult {
    pub item_id: String,
    pub bid_amount: u64,
    pub success: bool,
    pub message: String,
    pub seconds_remaining_at_bid: u64,
}

fn countdown_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)auction[^"']*(?:time|countdown)[^"']*["'](\d{1,2}:\d{2})["']"#).unwrap()
    })
}

fn mm_ss_to_secs(raw: &str) -> Option<u64> {
    let (m, s) = raw.split_once(':')?;
    Some(m.parse::<u64>().ok()? * 60 + s.parse::<u64>().ok()?)
}

/// Seconds until the next clock boundary for hour/day/week auctions.
pub fn seconds_until_auction_close(auction_type: AuctionType) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let period = match auction_type {
        AuctionType::Hour => 3600,
        AuctionType::Day => 86_400,
        AuctionType::Week => 604_800,
    };
    period - (now % period)
}

/// Parse a countdown from auction HTML when present; otherwise use the clock boundary.
pub fn auction_seconds_remaining(page: &str, auction_type: AuctionType) -> u64 {
    countdown_re()
        .captures(page)
        .and_then(|c| mm_ss_to_secs(&c[1]))
        .unwrap_or_else(|| seconds_until_auction_close(auction_type))
}

fn table_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(itemKey="item_[\S\s]+?)</tr>"#).unwrap())
}

fn row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)",
            r#"itemKey="item_[a-zA-Z]+_(.+?)".*?"#,
            r#"auction_item_name_col">\s+(.+?)\s+<.*?"#,
            r#"auction_item_type">\s+(.+?)\s+<.*?"#,
            r#"auction_item_highest" (.+?)>.*?"#,
            r#"auction_item_current">\s+(.+?)\s+<.*?"#,
            r#"auction_item_you">\s+(.+?)\s+<.*?"#,
            r#"item_[a-zA-Z]+_\d+_buyPrice" value="(.*?)".*?"#,
            r#"item_[a-zA-Z]+_\d+_lootId" value="(.*?)""#,
        ))
        .unwrap()
    })
}

fn user_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"showUser="(.+?)""#).unwrap())
}

fn error_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)infoText = '(.*?)';.*?icon = '(.*)';").unwrap())
}

fn parse_credits(s: &str) -> u64 {
    s.replace([',', '.', '-'], "").parse().unwrap_or(0)
}

/// Port of `AuctionData.buildAuctionItems`.
fn parse_row(row: &str) -> Option<AuctionItem> {
    if row.contains("value=\"\"")
        || !row.contains("itemKey")
        || !row.contains("auction_item_name_col")
        || !row.contains("auction_item_type")
        || !row.contains("auction_item_highest")
        || !row.contains("auction_item_current")
        || !row.contains("auction_item_you")
        || !row.contains("buyPrice")
        || !row.contains("lootId")
    {
        return None;
    }
    let caps = row_re().captures(row)?;
    let id = &caps[1];
    let auction_type = if row.contains("hour") {
        AuctionType::Hour
    } else if row.contains("day") {
        AuctionType::Day
    } else if row.contains("week") {
        AuctionType::Week
    } else {
        return None;
    };
    let highest_bidder_id = user_re()
        .captures(&caps[4])
        .map(|c| base62::decode(&c[1]))
        .unwrap_or(1);
    Some(AuctionItem {
        id: format!("item_{}_{}", auction_type.id(), id),
        loot_id: caps[8].to_string(),
        name: caps[2].trim().to_string(),
        item_type: caps[3].trim().to_string(),
        auction_type,
        highest_bidder_id,
        current_bid: parse_credits(&caps[5]),
        own_bid: parse_credits(&caps[6]),
        instant_buy: parse_credits(&caps[7]),
    })
}

/// Port of `AuctionData.parse`.
pub fn parse_auction_page(page: &str) -> Vec<AuctionItem> {
    table_re()
        .captures_iter(page)
        .filter_map(|c| parse_row(c.get(0).unwrap().as_str()))
        .collect()
}

impl BackpageClient {
    /// Scrape `internalAuction` into hour/day/week items.
    pub async fn auction_list(&self) -> Result<Vec<AuctionItem>> {
        let resp = self
            .get("indexInternal.es?action=internalAuction", &[])
            .await?;
        if resp.body.contains("redirectToLogin") || resp.status == reqwest::StatusCode::FOUND {
            return Err(Error::SessionExpired);
        }
        Ok(parse_auction_page(&resp.body))
    }

    /// Fetch auction page and return parsed seconds remaining for one auction type.
    pub async fn auction_seconds_remaining_for(&self, auction_type: AuctionType) -> Result<u64> {
        let resp = self
            .get("indexInternal.es?action=internalAuction", &[])
            .await?;
        if resp.body.contains("redirectToLogin") || resp.status == reqwest::StatusCode::FOUND {
            return Err(Error::SessionExpired);
        }
        Ok(auction_seconds_remaining(&resp.body, auction_type))
    }

    /// Poll until `snipe_at_secs` before close, then bid up to `max_credits`.
    pub async fn auction_snipe(
        &self,
        item_id: &str,
        max_credits: u64,
        snipe_at_secs: u64,
        bid_increment: u64,
        poll_interval: Duration,
    ) -> Result<AuctionSnipeResult> {
        loop {
            let items = self.auction_list().await?;
            let item = items
                .iter()
                .find(|i| i.id == item_id)
                .ok_or_else(|| Error::Api(format!("no auction item {item_id}")))?
                .clone();
            let remaining = self
                .auction_seconds_remaining_for(item.auction_type)
                .await?;

            if remaining <= snipe_at_secs {
                let bid_amount = (item.current_bid + bid_increment).min(max_credits);
                if bid_amount <= item.current_bid {
                    return Ok(AuctionSnipeResult {
                        item_id: item_id.to_string(),
                        bid_amount,
                        success: false,
                        message: format!(
                            "max credits {max_credits} below current bid {}",
                            item.current_bid
                        ),
                        seconds_remaining_at_bid: remaining,
                    });
                }
                let (success, message) = self.auction_bid(&item, bid_amount).await?;
                return Ok(AuctionSnipeResult {
                    item_id: item_id.to_string(),
                    bid_amount,
                    success,
                    message,
                    seconds_remaining_at_bid: remaining,
                });
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// `AuctionManager.bidItem` — fetch reloadToken, then POST `subAction=bid`.
    /// Returns `(success, server message)`.
    pub async fn auction_bid(&self, item: &AuctionItem, credits: u64) -> Result<(bool, String)> {
        let token = self
            .fetch_reload_token("indexInternal.es?action=internalAuction")
            .await?;
        let resp = self
            .post(
                "indexInternal.es",
                &[
                    Param::new("action", "internalAuction"),
                    Param::new("reloadToken", token),
                    Param::new("auctionType", item.auction_type.id()),
                    Param::new("subAction", "bid"),
                    Param::new("lootId", &item.loot_id),
                    Param::new("itemId", &item.id),
                    Param::new("credits", credits.to_string()),
                    Param::new("auction_buy_button", "BID"),
                ],
            )
            .await?;
        match error_re().captures(&resp.body) {
            Some(caps) => Ok((!caps[2].contains("error"), caps[1].to_string())),
            None => Err(Error::Parse("no infoText in bid response".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_credits() {
        assert_eq!(parse_credits("1.234.567"), 1234567);
        assert_eq!(parse_credits("-"), 0);
    }

    #[test]
    fn parses_mm_ss_countdown() {
        assert_eq!(mm_ss_to_secs("02:14"), Some(134));
    }

    #[test]
    fn parses_auction_row() {
        let row = r#"<tr class="auctionItemRow " itemGroup="battery" itemKey="item_hour_0" onclick="Auction.showItem(this)">
                <td class="auction_item_name_col">
                    MCB-25
                </td>
                <td class="auction_item_type">
                    Pil
                </td>
                <td class="auction_item_highest" showUser="6tTIT" title="x">
                    bidder
                </td>
                <td class="auction_item_current">
                    100.000
                </td>
                <td class="auction_item_you">
                    0
                </td>
                <td class="auction_item_instant">
                    <input type="hidden" id="item_hour_0_buyPrice" value="475" />
                    <input type="hidden" id="item_hour_0_lootId" value="ammunition_laser_mcb-25" />
                </td>
            </tr>"#;
        let item = parse_row(row).expect("row should parse");
        assert_eq!(item.id, "item_hour_0");
        assert_eq!(item.name, "MCB-25");
        assert_eq!(item.current_bid, 100_000);
        assert_eq!(item.loot_id, "ammunition_laser_mcb-25");
    }
}
