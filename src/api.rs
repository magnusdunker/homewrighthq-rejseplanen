//! Rejseplanen's API 2.0 (HAFAS ReST at www.rejseplanen.dk/api), the two
//! services this integration uses: `location.name` to find a stop and
//! `multiDepartureBoard` for the departures from several stops in one call.
//!
//! Every call carries the user's `accessId` and asks for JSON. Times in the
//! answers are Danish local time.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Tz;
use serde_json::Value;

pub const BASE_URL: &str = "https://www.rejseplanen.dk/api";
pub const TZ: Tz = chrono_tz::Europe::Copenhagen;

#[derive(Debug)]
pub enum ApiError {
    /// The access ID is wrong, expired or not allowed this service.
    Auth(String),
    /// An error the API reported, e.g. an unknown stop.
    Api { code: String, text: String },
    /// No answer, or an answer that is not what the API sends.
    Net(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth(t) => write!(f, "Rejseplanen afviste adgangsnøglen: {t}"),
            ApiError::Api { code, text } => write!(f, "Rejseplanen: {code} {text}"),
            ApiError::Net(t) => write!(f, "Rejseplanen svarer ikke: {t}"),
        }
    }
}

/// A stop from `location.name`.
#[derive(Clone, Debug, PartialEq)]
pub struct Stop {
    /// `extId`, the number Rejseplanen uses for the stop.
    pub id: String,
    pub name: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Departure {
    /// The full name, e.g. "Bus 2A" or "IC 845".
    pub name: String,
    /// The line, e.g. "2A", "A" for an S-train or "M3".
    pub line: String,
    /// Rejseplanen's category, e.g. "Bus", "S-Tog", "IC" or "MET".
    pub category: String,
    /// The product class bit (1 IC, 2 ICL, 4 Re, 8 other trains, 16 S-train,
    /// 32 bus, 64 express bus, 128 night bus, 256 flex, 512 ferry, 1024
    /// metro, 2048 light rail, 4096 flight), 0 if unknown.
    pub class: u32,
    pub direction: String,
    pub stop: String,
    pub stop_id: String,
    /// The main stop a platform belongs to, when it is one.
    pub main_stop_id: Option<String>,
    pub planned: DateTime<Tz>,
    pub realtime: Option<DateTime<Tz>>,
    pub track: Option<String>,
    pub realtime_track: Option<String>,
    pub cancelled: bool,
    pub part_cancelled: bool,
}

impl Departure {
    /// When it leaves: the real-time estimate if there is one.
    pub fn time(&self) -> DateTime<Tz> {
        self.realtime.unwrap_or(self.planned)
    }

    /// Minutes late, never negative; `None` without real-time data.
    pub fn delay_minutes(&self) -> Option<i64> {
        self.realtime.map(|rt| ((rt - self.planned).num_seconds() as f64 / 60.0).round().max(0.0) as i64)
    }

    pub fn track(&self) -> Option<&str> {
        self.realtime_track.as_deref().or(self.track.as_deref())
    }
}

pub struct Client {
    base: String,
    access_id: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: &str, access_id: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("homewright-rejseplanen/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self { base: base.trim_end_matches('/').to_string(), access_id: access_id.trim().to_string(), http }
    }

    async fn get(&self, service: &str, params: &[(&str, String)]) -> Result<Value, ApiError> {
        let url = format!("{}/{service}", self.base);
        let mut query: Vec<(&str, String)> = vec![("accessId", self.access_id.clone()), ("format", "json".into())];
        query.extend_from_slice(params);
        let resp = self.http.get(&url).query(&query).send().await.map_err(|e| ApiError::Net(e.without_url().to_string()))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| ApiError::Net(e.without_url().to_string()))?;
        let json: Option<Value> = serde_json::from_str(&body).ok();
        if let Some(e) = json.as_ref().and_then(api_error) {
            return Err(e);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ApiError::Auth(format!("HTTP {status}")));
        }
        if !status.is_success() {
            return Err(ApiError::Net(format!("HTTP {status}")));
        }
        json.ok_or_else(|| ApiError::Net("svaret er ikke JSON".into()))
    }

    /// Stops whose name matches `query`, best match first.
    pub async fn search(&self, query: &str) -> Result<Vec<Stop>, ApiError> {
        let v = self.get("location.name", &[("input", query.to_string()), ("type", "S".into()), ("maxNo", "20".into())]).await?;
        Ok(parse_stops(&v))
    }

    /// Departures from `stop_ids` in the next `minutes` minutes.
    pub async fn departures(&self, stop_ids: &[String], minutes: u32) -> Result<Vec<Departure>, ApiError> {
        let v = self.get("multiDepartureBoard", &[("idList", stop_ids.join("|")), ("duration", minutes.to_string())]).await?;
        Ok(parse_departures(&v))
    }
}

/// The error in an answer such as
/// `{"errorCode":"API_AUTH","errorText":"access denied for ..."}`.
pub fn api_error(v: &Value) -> Option<ApiError> {
    let code = v.get("errorCode")?.as_str()?.to_string();
    let text = v.get("errorText").and_then(Value::as_str).unwrap_or_default().to_string();
    Some(if code == "API_AUTH" { ApiError::Auth(text) } else { ApiError::Api { code, text } })
}

/// A JSON value as a list: HAFAS sends one element as an object in some
/// places and as a list of one in others.
fn list(v: Option<&Value>) -> Vec<&Value> {
    match v {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(v @ Value::Object(_)) => vec![v],
        _ => vec![],
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn number(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn flag(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

pub fn parse_stops(v: &Value) -> Vec<Stop> {
    // `stopLocationOrCoordLocation` holds `{"StopLocation": {...}}` and
    // `{"CoordLocation": {...}}`; older answers have `StopLocation` at the top
    let mut items: Vec<&Value> = list(v.get("stopLocationOrCoordLocation")).into_iter().flat_map(|i| list(i.get("StopLocation"))).collect();
    items.extend(list(v.get("StopLocation")));
    items
        .into_iter()
        .filter_map(|s| {
            Some(Stop {
                id: text(s, "extId").or_else(|| text(s, "id").filter(|i| i.chars().all(|c| c.is_ascii_digit())))?,
                name: text(s, "name")?,
                lat: number(s, "lat"),
                lon: number(s, "lon"),
            })
        })
        .collect()
}

fn local(date: &str, time: &str) -> Option<DateTime<Tz>> {
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let t = NaiveTime::parse_from_str(time, "%H:%M:%S").or_else(|_| NaiveTime::parse_from_str(time, "%H:%M")).ok()?;
    // the hour that repeats when summer time ends: the first of the two
    TZ.from_local_datetime(&d.and_time(t)).earliest()
}

pub fn parse_departures(v: &Value) -> Vec<Departure> {
    let mut out: Vec<Departure> = list(v.get("Departure"))
        .into_iter()
        .filter_map(|d| {
            let date = text(d, "date")?;
            let planned = local(&date, &text(d, "time")?)?;
            let realtime = text(d, "rtTime").and_then(|t| local(&text(d, "rtDate").unwrap_or(date.clone()), &t));
            // ProductAtStop describes the service at this stop; Product along the way
            let product = d.get("ProductAtStop").or_else(|| list(d.get("Product")).into_iter().next()).cloned().unwrap_or(Value::Null);
            let name = text(d, "name").or_else(|| text(&product, "name")).unwrap_or_default();
            let line = text(&product, "line").or_else(|| text(&product, "displayNumber")).or_else(|| text(&product, "num")).unwrap_or_else(|| name.clone());
            Some(Departure {
                line,
                category: text(&product, "catOut").or_else(|| text(&product, "catOutS")).unwrap_or_default(),
                class: number(&product, "cls").map(|c| c as u32).unwrap_or(0),
                direction: text(d, "direction").unwrap_or_default(),
                stop: text(d, "stop").unwrap_or_default(),
                stop_id: text(d, "stopExtId").unwrap_or_default(),
                main_stop_id: text(d, "mainMastExtId"),
                planned,
                realtime,
                track: text(d, "track"),
                realtime_track: text(d, "rtTrack"),
                cancelled: flag(d, "cancelled"),
                part_cancelled: flag(d, "partCancelled"),
                name,
            })
        })
        .collect();
    out.sort_by_key(Departure::time);
    out
}
