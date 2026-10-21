//! Departure boards from Rejseplanen as an external integration for
//! HomewrightHQ.
//!
//! Each setup is one stop with its own filters. Setups with the same access
//! ID share one poller that asks for all their stops in a single
//! `multiDepartureBoard` call: once a minute, every five minutes between
//! 01:00 and 05:00. A private access ID allows 50,000 calls a month, and
//! that schedule uses about 39,000 however many stops there are.

pub mod api;
pub mod board;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use thehome_plugin::{async_trait, json, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};
use tokio::sync::Notify;

pub use api::{ApiError, Client, Departure, Stop, BASE_URL, TZ};
use board::{Filter, KINDS};

pub const DOMAIN: &str = "rejseplanen";
/// How far ahead each board looks, in minutes.
const BOARD_MINUTES: u32 = 120;
const DAY_POLL: StdDuration = StdDuration::from_secs(60);
const NIGHT_POLL: StdDuration = StdDuration::from_secs(300);
/// After this long without a fresh board the sensors become unavailable.
const STALE: StdDuration = StdDuration::from_secs(30 * 60);
/// The search used to check an access ID.
const PROBE: &str = "Nørreport";

/// Logging to HomewrightHQ's log through stderr.
#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    thehome_plugin::log(level, message);
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::emit("INFO", format!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::emit("WARN", format!($($t)*)) };
}

fn poll_interval(now: DateTime<Tz>) -> StdDuration {
    if (1..5).contains(&now.hour()) {
        NIGHT_POLL
    } else {
        DAY_POLL
    }
}

fn access_schema() -> Value {
    json!({
        "type": "object",
        "required": ["access_id"],
        "properties": {
            "access_id": {
                "type": "string", "format": "password", "title": "Adgangsnøgle (accessId)",
                "description": "Søg om en gratis nøgle hos Rejseplanen Labs; se integrationens README"
            }
        }
    })
}

fn search_schema(query: &str) -> Value {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string", "title": "Stoppested", "default": query, "description": "Navnet eller en del af det, f.eks. Aarhus H eller Nørreport St." }
        }
    })
}

fn stop_schema(stops: &[Stop]) -> Value {
    let options: Vec<Value> = stops.iter().map(|s| json!({ "const": s.id, "title": format!("{} ({})", s.name, s.id) })).collect();
    json!({
        "type": "object",
        "required": ["stop"],
        "properties": { "stop": { "type": "string", "title": "Stoppested", "oneOf": options, "default": stops[0].id } }
    })
}

fn filter_schema(f: &Filter) -> Value {
    let kinds: Vec<Value> = KINDS.iter().map(|(k, name, _)| json!({ "const": k, "title": name })).collect();
    json!({
        "type": "object",
        "properties": {
            "kinds": {
                "type": "array", "title": "Transportmidler", "items": { "type": "string", "oneOf": kinds }, "default": f.kinds,
                "description": "Ingen valgt viser alle"
            },
            "lines": {
                "type": "string", "title": "Linjer", "default": f.lines.join(", "),
                "description": "Adskilt af komma, f.eks. 2A, 18 eller A, B. Tom viser alle"
            },
            "directions": {
                "type": "string", "title": "Retning", "default": f.directions.join(", "),
                "description": "Tekst, som retningen skal indeholde, f.eks. Skejby. Flere adskilles med komma. Tom viser alle"
            }
        }
    })
}

fn with_description(mut r: FlowReply, text: &str) -> FlowReply {
    r.result["description"] = json!(text);
    r
}

fn stops_of(v: Option<&Value>) -> Vec<Stop> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| Some(Stop { id: s["id"].as_str()?.into(), name: s["name"].as_str()?.into(), lat: s["lat"].as_f64(), lon: s["lon"].as_f64() }))
        .collect()
}

/// `invalid_auth` or `cannot_connect` for a form.
fn form_error(e: &ApiError) -> &'static str {
    if matches!(e, ApiError::Auth(_)) {
        "invalid_auth"
    } else {
        "cannot_connect"
    }
}

/// The filter of an entry: its options, otherwise what the setup chose.
fn entry_filter(entry: &Entry) -> Filter {
    if entry.options.as_object().is_some_and(|o| !o.is_empty()) {
        Filter::from_options(&entry.options)
    } else {
        Filter::from_options(&entry.data["filter"])
    }
}

/// One stop the poller keeps up to date.
struct Watch {
    stop_id: String,
    filter: Filter,
    next: Option<String>,
    minutes: Option<String>,
    /// What was sent last, so an unchanged state is not sent again.
    sent: HashMap<String, (String, Map<String, Value>)>,
}

#[derive(Default)]
struct Board {
    departures: Vec<Departure>,
    fetched: Option<Instant>,
    /// The access ID was refused on the last try.
    refused: bool,
}

struct Poller {
    client: Client,
    watches: Mutex<HashMap<String, Watch>>,
    board: Mutex<Board>,
    /// Ask again now, e.g. for a stop that was just added.
    wake: Notify,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Poller {
    fn stop_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.watches.lock().unwrap().values().map(|w| w.stop_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    async fn fetch(&self, host: &Host) {
        let ids = self.stop_ids();
        if ids.is_empty() {
            return;
        }
        let result = self.client.departures(&ids, BOARD_MINUTES).await;
        let entries: Vec<String> = self.watches.lock().unwrap().keys().cloned().collect();
        match result {
            Ok(d) => {
                let was_refused = {
                    let mut b = self.board.lock().unwrap();
                    b.departures = d;
                    b.fetched = Some(Instant::now());
                    std::mem::replace(&mut b.refused, false)
                };
                if was_refused {
                    for e in &entries {
                        let _ = host.delete_issue(e, "auth").await;
                    }
                }
            }
            Err(ApiError::Auth(t)) => {
                warn!("{t}");
                self.board.lock().unwrap().refused = true;
                for e in &entries {
                    let _ = host
                        .create_issue(
                            e,
                            "auth",
                            "error",
                            "Rejseplanen: adgangsnøglen blev afvist",
                            "Rejseplanen afviser integrationens adgangsnøgle. Den kan være udløbet eller lukket. Godkend integrationen igen med en gyldig nøgle.",
                        )
                        .await;
                }
            }
            Err(e) => warn!("{e}"),
        }
    }

    /// Send each stop's states, computed from the board at `now`.
    async fn publish(&self, host: &Host, now: DateTime<Tz>) {
        let (fresh, departures) = {
            let b = self.board.lock().unwrap();
            (b.fetched.is_some_and(|t| t.elapsed() < STALE) && !b.refused, b.departures.clone())
        };
        let mut out: Vec<(String, Option<board::State>)> = vec![];
        {
            let mut watches = self.watches.lock().unwrap();
            for w in watches.values_mut() {
                let up = board::upcoming(&departures, &w.stop_id, &w.filter, now);
                let (next, minutes) = board::states(&up, now);
                for (id, state) in [(&w.next, next), (&w.minutes, minutes)] {
                    let Some(id) = id else { continue };
                    let new = fresh.then_some(state);
                    let unchanged = match &new {
                        Some(s) => w.sent.get(id) == Some(s),
                        None => w.sent.get(id).is_some_and(|(s, _)| s == "unavailable"),
                    };
                    if !unchanged {
                        w.sent.insert(id.clone(), new.clone().unwrap_or(("unavailable".into(), Map::new())));
                        out.push((id.clone(), new));
                    }
                }
            }
        }
        for (id, state) in out {
            let r = match state {
                Some((s, a)) => host.set_state(&id, s, a).await,
                None => host.set_unavailable(&id).await,
            };
            if let Err(e) = r {
                warn!("kunne ikke opdatere {id}: {e:?}");
            }
        }
    }

    async fn run(self: Arc<Self>, host: Host) {
        let mut last: Option<Instant> = self.board.lock().unwrap().fetched;
        let mut forced = false;
        loop {
            let now = Utc::now().with_timezone(&TZ);
            if forced || last.is_none_or(|t| t.elapsed() + StdDuration::from_secs(1) >= poll_interval(now)) {
                self.fetch(&host).await;
                last = Some(Instant::now());
            }
            self.publish(&host, Utc::now().with_timezone(&TZ)).await;
            // to the start of the next minute, where the minutes change
            let s = u64::from(Utc::now().second());
            let pause = StdDuration::from_secs(60 - s.min(59));
            tokio::select! {
                _ = tokio::time::sleep(pause) => forced = false,
                _ = self.wake.notified() => forced = true,
            }
        }
    }
}

#[derive(Default)]
pub struct Rejseplanen {
    /// Pollers by base URL and access ID.
    pollers: Mutex<HashMap<String, Arc<Poller>>>,
    /// The poller each entry is in.
    entries: Mutex<HashMap<String, String>>,
}

impl Rejseplanen {
    fn leave(&self, entry_id: &str) {
        let Some(key) = self.entries.lock().unwrap().remove(entry_id) else { return };
        let mut pollers = self.pollers.lock().unwrap();
        let Some(p) = pollers.get(&key).cloned() else { return };
        let empty = {
            let mut w = p.watches.lock().unwrap();
            w.remove(entry_id);
            w.is_empty()
        };
        if empty {
            pollers.remove(&key);
            if let Some(t) = p.task.lock().unwrap().take() {
                t.abort();
            }
        }
    }
}

#[async_trait]
impl Integration for Rejseplanen {
    async fn config_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let base = flow.data.get("base_url").and_then(Value::as_str).unwrap_or(BASE_URL).to_string();
        let mut data = flow.data.clone();
        match (step, input) {
            ("user", None) | ("reauth", None) => Ok(with_description(
                FlowReply::form("user", access_schema()),
                "Integrationen bruger Rejseplanens API 2.0, som kræver en adgangsnøgle. Den er gratis for private.",
            )),
            ("user", Some(v)) => {
                let access = v["access_id"].as_str().unwrap_or_default().trim().to_string();
                if let Err(e) = Client::new(&base, &access).search(PROBE).await {
                    info!("{e}");
                    return Ok(FlowReply::form_with_errors("user", access_schema(), json!({ "base": form_error(&e) })));
                }
                if flow.source == "reauth" {
                    let mut d = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
                    d.insert("access_id".into(), json!(access));
                    return Ok(FlowReply::create("", Value::Object(d)));
                }
                data.insert("access_id".into(), json!(access));
                Ok(FlowReply::form("search", search_schema("")).with_data(data))
            }
            ("search", Some(v)) => {
                let query = v["query"].as_str().unwrap_or_default().trim().to_string();
                let access = data.get("access_id").and_then(Value::as_str).unwrap_or_default().to_string();
                let stops = match Client::new(&base, &access).search(&query).await {
                    Ok(s) => s,
                    Err(e) => {
                        info!("{e}");
                        return Ok(FlowReply::form_with_errors("search", search_schema(&query), json!({ "base": form_error(&e) })));
                    }
                };
                if query.is_empty() || stops.is_empty() {
                    return Ok(FlowReply::form_with_errors("search", search_schema(&query), json!({ "query": "no_stops" })));
                }
                data.insert("_stops".into(), stops.iter().map(|s| json!({ "id": s.id, "name": s.name, "lat": s.lat, "lon": s.lon })).collect());
                let r = FlowReply::form("stop", stop_schema(&stops)).with_data(data);
                Ok(with_description(r, "Samme navn kan høre til flere stoppesteder, f.eks. et for toget og et for togbussen. Nummeret skiller dem ad."))
            }
            ("stop", Some(v)) => {
                let stops = stops_of(data.remove("_stops").as_ref());
                let id = v["stop"].as_str().unwrap_or_default();
                let stop = stops.into_iter().find(|s| s.id == id).ok_or_else(|| Error::invalid("ukendt stoppested"))?;
                data.insert("stop_id".into(), json!(stop.id));
                data.insert("stop_name".into(), json!(stop.name));
                if let (Some(lat), Some(lon)) = (stop.lat, stop.lon) {
                    data.insert("latitude".into(), json!(lat));
                    data.insert("longitude".into(), json!(lon));
                }
                let r = FlowReply::form("filters", filter_schema(&Filter::default())).with_data(data).with_unique_id(stop.id);
                Ok(with_description(r, "Vælg, hvilke afgange der skal med. Det kan ændres senere under integrationens indstillinger."))
            }
            ("filters", Some(v)) => {
                let filter = Filter::from_options(&v);
                data.insert("filter".into(), filter.to_options());
                let title = data.get("stop_name").and_then(Value::as_str).unwrap_or("Rejseplanen").to_string();
                Ok(FlowReply::create(&title, Value::Object(data)))
            }
            (s, _) => Err(Error::invalid(format!("ukendt trin {s}"))),
        }
    }

    async fn options_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        match input {
            None => {
                let filter = flow.entry.as_ref().map(entry_filter).unwrap_or_default();
                Ok(FlowReply::form("filters", filter_schema(&filter)))
            }
            Some(v) if step == "filters" => Ok(FlowReply::create("", Filter::from_options(&v).to_options())),
            Some(_) => Err(Error::invalid(format!("ukendt trin {step}"))),
        }
    }

    async fn setup_entry(&self, host: &Host, entry: Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let access = entry.get_str("access_id").ok_or_else(|| Error::auth_failed("adgangsnøglen mangler"))?.to_string();
        let stop_id = entry.get_str("stop_id").ok_or_else(|| Error::failed("intet stoppested"))?.to_string();
        let stop_name = entry.get_str("stop_name").unwrap_or(&stop_id).to_string();
        let base = entry.get_str("base_url").unwrap_or(BASE_URL).to_string();
        let filter = entry_filter(&entry);
        self.leave(&entry_id);

        let key = format!("{base} {access}");
        let existing = self.pollers.lock().unwrap().get(&key).cloned();
        // A new poller asks once before the entry counts as set up, so a
        // refused access ID shows up as a login to renew.
        let poller = match existing {
            Some(p) => p,
            None => {
                let client = Client::new(&base, &access);
                let first = match client.departures(std::slice::from_ref(&stop_id), BOARD_MINUTES).await {
                    Ok(d) => d,
                    Err(ApiError::Auth(t)) => return Err(Error::auth_failed(t)),
                    Err(e) => return Err(Error::not_ready(e)),
                };
                let board = Board { departures: first, fetched: Some(Instant::now()), refused: false };
                Arc::new(Poller { client, watches: Mutex::default(), board: Mutex::new(board), wake: Notify::new(), task: Mutex::default() })
            }
        };

        let device = json!({
            "identifiers": [[DOMAIN, stop_id]],
            "name": stop_name,
            "manufacturer": "Rejseplanen",
            "model": "Afgangstavle",
            "serial_number": stop_id,
            "entry_type": "service",
            "configuration_url": "https://www.rejseplanen.dk/",
        });
        let dev = host.request("device/register", json!({ "entry_id": entry_id, "device": device })).await?;
        let dev_id = dev["id"].as_str().ok_or_else(|| Error::failed("intet enheds-id"))?.to_string();
        let add = |key: &str, name: &str, class: &str, unit: Option<&str>, icon: &str| {
            let mut entity = json!({
                "domain": "sensor", "unique_id": format!("{stop_id}-{key}"), "name": name, "device_id": dev_id,
                "device_class": class, "icon": icon,
            });
            if let Some(u) = unit {
                entity["unit_of_measurement"] = json!(u);
            }
            let params = json!({ "entry_id": entry_id, "entity": entity, "handler": false });
            async move { host.request("entity/add", params).await.map(|v| v["entity_id"].as_str().map(String::from)) }
        };
        let next = add("next", "Næste afgang", "timestamp", None, "mdi:bus-clock").await?;
        let minutes = add("minutes", "Minutter til afgang", "duration", Some("min"), "mdi:timer-outline").await?;

        let watch = Watch { stop_id, filter, next, minutes, sent: HashMap::new() };
        poller.watches.lock().unwrap().insert(entry_id.clone(), watch);
        let joined = poller.task.lock().unwrap().is_some();
        self.entries.lock().unwrap().insert(entry_id, key.clone());
        self.pollers.lock().unwrap().insert(key, poller.clone());
        if joined {
            // a stop added to a running poller: ask again for all of them
            poller.wake.notify_one();
        } else {
            let task = tokio::spawn(poller.clone().run(host.clone()));
            *poller.task.lock().unwrap() = Some(task);
        }
        Ok(())
    }

    async fn unload_entry(&self, _host: &Host, entry_id: &str) -> Result<(), Error> {
        self.leave(entry_id);
        Ok(())
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = entry.data.clone();
        if let Value::Object(m) = &mut d {
            m.insert("access_id".into(), json!("**REDACTED**"));
        }
        let key = self.entries.lock().unwrap().get(&entry.entry_id).cloned();
        let poller = key.and_then(|k| self.pollers.lock().unwrap().get(&k).cloned());
        let board = poller.map(|p| {
            let b = p.board.lock().unwrap();
            json!({
                "departures": b.departures.len(),
                "age_seconds": b.fetched.map(|t| t.elapsed().as_secs()),
                "refused": b.refused,
                "stops": p.stop_ids(),
            })
        });
        json!({ "entry": d, "options": entry.options, "board": board })
    }
}
