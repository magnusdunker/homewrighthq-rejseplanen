//! From the departures of one call to the state of a stop's sensors: the
//! user's filters, the next departure and the list after it.

use chrono::{DateTime, Duration};
use chrono_tz::Tz;
use thehome_plugin::{json, Map, Value};

use crate::api::Departure;

/// How many departures the attribute `departures` holds.
pub const LIST_LEN: usize = 10;

/// The transport types in the filter: key, Danish name and product classes.
pub const KINDS: &[(&str, &str, u32)] = &[
    ("bus", "Bus", 32 | 64 | 128 | 256),
    ("tog", "Tog (IC, Lyn og regionaltog)", 1 | 2 | 4 | 8),
    ("s_tog", "S-tog", 16),
    ("metro", "Metro", 1024),
    ("letbane", "Letbane", 2048),
    ("faerge", "Færge", 512),
];

/// The class of a departure without one, guessed from its category.
fn class_of(d: &Departure) -> u32 {
    if d.class != 0 {
        return d.class;
    }
    let c = d.category.to_lowercase();
    match c.as_str() {
        "s-tog" | "s" => 16,
        "met" | "m" | "metro" => 1024,
        "letbane" | "let" | "ltbus" => 2048,
        "færge" | "hf" => 512,
        _ if c.contains("bus") || c == "flextur" => 32,
        _ => 8,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// Lines to show, e.g. "2A" or "A"; empty shows all.
    pub lines: Vec<String>,
    /// Text the direction must contain, e.g. "Skejby"; empty shows all.
    pub directions: Vec<String>,
    /// Keys from [`KINDS`]; empty shows all.
    pub kinds: Vec<String>,
}

fn words(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        Value::String(s) => s.split([',', ';', '\n']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        _ => vec![],
    }
}

impl Filter {
    /// From an entry's options (or the form that sets them).
    pub fn from_options(o: &Value) -> Self {
        Self {
            lines: words(&o["lines"]),
            directions: words(&o["directions"]),
            kinds: words(&o["kinds"]).into_iter().filter(|k| KINDS.iter().any(|(key, _, _)| key == k)).collect(),
        }
    }

    pub fn to_options(&self) -> Value {
        json!({ "lines": self.lines.join(", "), "directions": self.directions.join(", "), "kinds": self.kinds })
    }

    fn mask(&self) -> u32 {
        KINDS.iter().filter(|(k, _, _)| self.kinds.iter().any(|s| s == k)).fold(0, |m, (_, _, c)| m | c)
    }

    pub fn matches(&self, d: &Departure) -> bool {
        let mask = self.mask();
        if mask != 0 && class_of(d) & mask == 0 {
            return false;
        }
        if !self.lines.is_empty() && !self.lines.iter().any(|l| l.eq_ignore_ascii_case(&d.line) || l.eq_ignore_ascii_case(&d.name)) {
            return false;
        }
        let dir = d.direction.to_lowercase();
        self.directions.is_empty() || self.directions.iter().any(|w| dir.contains(&w.to_lowercase()))
    }
}

/// A departure counts until half a minute after it leaves.
const GRACE: i64 = 30;

/// The departures from `stop_id` (or one of its platforms) that pass the
/// filter and have not left at `now`, in order.
pub fn upcoming<'a>(all: &'a [Departure], stop_id: &str, filter: &Filter, now: DateTime<Tz>) -> Vec<&'a Departure> {
    let mut out: Vec<&Departure> = all
        .iter()
        .filter(|d| d.stop_id == stop_id || d.main_stop_id.as_deref() == Some(stop_id))
        .filter(|d| filter.matches(d))
        .filter(|d| d.time() >= now - Duration::seconds(GRACE))
        .collect();
    out.sort_by_key(|d| d.time());
    out
}

fn describe(d: &Departure) -> Value {
    json!({
        "line": d.line,
        "name": d.name,
        "type": d.category,
        "direction": d.direction,
        "planned": d.planned.to_rfc3339(),
        "realtime": d.realtime.map(|t| t.to_rfc3339()),
        "delay": d.delay_minutes(),
        "track": d.track(),
        "cancelled": d.cancelled,
        "part_cancelled": d.part_cancelled,
    })
}

/// A sensor's state and attributes.
pub type State = (String, Map<String, Value>);

/// The sensors' states: for "Næste afgang" and for
/// "Minutter til afgang". The next departure is the first one not cancelled.
pub fn states(upcoming: &[&Departure], now: DateTime<Tz>) -> (State, State) {
    let next = upcoming.iter().find(|d| !d.cancelled);
    let mut a = Map::new();
    let mut m = Map::new();
    if let Some(d) = next {
        for (k, v) in [
            ("line", json!(d.line)),
            ("name", json!(d.name)),
            ("type", json!(d.category)),
            ("direction", json!(d.direction)),
            ("track", json!(d.track())),
            ("planned", json!(d.planned.to_rfc3339())),
            ("delay", json!(d.delay_minutes())),
        ] {
            a.insert(k.into(), v.clone());
            if matches!(k, "line" | "direction" | "track" | "delay") {
                m.insert(k.into(), v);
            }
        }
    }
    a.insert("departures".into(), Value::Array(upcoming.iter().take(LIST_LEN).map(|d| describe(d)).collect()));
    let Some(d) = next else {
        return (("unknown".into(), a), ("unknown".into(), m));
    };
    let minutes = ((d.time() - now).num_seconds().max(0)) / 60;
    ((d.time().to_rfc3339(), a), (minutes.to_string(), m))
}
