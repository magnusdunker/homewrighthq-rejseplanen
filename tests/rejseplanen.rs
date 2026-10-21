//! The fixtures follow the JSON that Rejseplanen's API 2.0 sends. The plugin
//! tests play HomewrightHQ over a pipe against a fake API.

mod host;

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use homewright_rejseplanen::api::{parse_departures, parse_stops};
use homewright_rejseplanen::board::{states, upcoming, Filter};
use homewright_rejseplanen::{ApiError, Client, Rejseplanen, TZ};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "key-123";
const NORREPORT: &str = "8600646";

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn at(h: u32, m: u32) -> DateTime<Tz> {
    TZ.with_ymd_and_hms(2026, 10, 18, h, m, 0).unwrap()
}

#[test]
fn parse_stop_search() {
    let stops = parse_stops(&fixture("location.json"));
    assert_eq!(stops.len(), 2, "the CoordLocation is not a stop");
    assert_eq!(stops[0].id, NORREPORT);
    assert_eq!(stops[0].name, "Nørreport St.");
    assert_eq!(stops[0].lat, Some(55.683487));
    assert_eq!(stops[1].id, "8650646");
}

#[test]
fn parse_board() {
    let d = parse_departures(&fixture("departures.json"));
    assert_eq!(d.len(), 6);
    // sorted by when they leave, real time first
    let names: Vec<&str> = d.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["A", "Bus 5C", "Metro M3", "Metro M1", "IC 140", "Re 4530"]);

    let a = &d[0];
    assert_eq!((a.line.as_str(), a.category.as_str(), a.class), ("A", "S-Tog", 16));
    assert_eq!(a.planned, at(12, 30));
    assert_eq!(a.realtime, Some(at(12, 31)));
    assert_eq!(a.delay_minutes(), Some(1));
    assert_eq!(a.track(), Some("2"));

    let bus = &d[1];
    assert_eq!(bus.line, "5C");
    assert_eq!(bus.stop_id, "10103");
    assert_eq!(bus.main_stop_id.as_deref(), Some(NORREPORT));
    assert_eq!(bus.delay_minutes(), None);

    assert!(d[2].cancelled);
    // no `line`: the display number
    assert_eq!(d[5].line, "4530");
    assert_eq!(d[5].track(), Some("3"), "the real-time track wins");
    assert_eq!(d[5].delay_minutes(), Some(4));
}

#[test]
fn summer_time_ends() {
    // 02:30 happens twice on 2026-10-25; the first one is summer time
    let v = json!({ "Departure": [{ "name": "N", "time": "02:30:00", "date": "2026-10-25", "stopExtId": "1" }] });
    let d = parse_departures(&v);
    assert_eq!(d[0].planned.naive_utc().time(), NaiveTime::from_hms_opt(0, 30, 0).unwrap());
}

#[test]
fn filters() {
    let d = parse_departures(&fixture("departures.json"));
    let names = |f: &Filter, now| upcoming(&d, NORREPORT, f, now).iter().map(|d| d.name.clone()).collect::<Vec<_>>();

    // everything from Nørreport and its bus stop, not Aarhus
    assert_eq!(names(&Filter::default(), at(12, 0)), ["A", "Bus 5C", "Metro M3", "Metro M1", "Re 4530"]);
    // gone half a minute after it left; the S-train is late until 12:31
    assert_eq!(names(&Filter::default(), at(12, 31) + Duration::seconds(20))[0], "A");
    assert_eq!(names(&Filter::default(), at(12, 32))[0], "Metro M3");

    let metro = Filter::from_options(&json!({ "kinds": ["metro"] }));
    assert_eq!(names(&metro, at(12, 0)), ["Metro M3", "Metro M1"]);
    let trains = Filter::from_options(&json!({ "kinds": ["tog", "s_tog"] }));
    assert_eq!(names(&trains, at(12, 0)), ["A", "Re 4530"]);
    let lines = Filter::from_options(&json!({ "lines": "5c, m1" }));
    assert_eq!(names(&lines, at(12, 0)), ["Bus 5C", "Metro M1"]);
    let direction = Filter::from_options(&json!({ "directions": "hillerød,KALUNDBORG" }));
    assert_eq!(names(&direction, at(12, 0)), ["A", "Re 4530"]);
    // unknown kinds are dropped
    assert_eq!(Filter::from_options(&json!({ "kinds": ["rumfærge"] })), Filter::default());
}

#[test]
fn sensor_states() {
    let d = parse_departures(&fixture("departures.json"));
    let metro = Filter::from_options(&json!({ "kinds": ["metro"] }));
    let now = at(12, 29) + Duration::seconds(10);
    let up = upcoming(&d, NORREPORT, &metro, now);
    let ((next, attrs), (minutes, mattrs)) = states(&up, now);
    // M3 is cancelled, so the next departure is M1
    assert_eq!(next, at(12, 33).to_rfc3339());
    assert_eq!(minutes, "3");
    assert_eq!(attrs["line"], "M1");
    assert_eq!(attrs["direction"], "Vanløse St.");
    assert_eq!(mattrs["line"], "M1");
    let list = attrs["departures"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["line"], "M3");
    assert_eq!(list[0]["cancelled"], true);

    let none = upcoming(&d, NORREPORT, &metro, at(13, 0));
    let ((next, attrs), (minutes, _)) = states(&none, at(13, 0));
    assert_eq!((next.as_str(), minutes.as_str()), ("unknown", "unknown"));
    assert_eq!(attrs["departures"], json!([]));
}

async fn refuse_others(server: &MockServer) {
    // what the API answers a wrong accessId
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "serverVersion": "2.53.1.2", "dialectVersion": "2.53-Rejseplanen", "errorCode": "API_AUTH",
            "errorText": "access denied for wrong on location.name identified by location.name", "requestId": "x"
        })))
        .with_priority(10)
        .mount(server)
        .await;
}

async fn mock_api(server: &MockServer, board: Value) {
    Mock::given(method("GET")).and(path("/location.name")).and(query_param("accessId", KEY)).and(query_param("format", "json"))
        .and(query_param("input", "xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "serverVersion": "2.53.1.2", "requestId": "e" })))
        .mount(server).await;
    Mock::given(method("GET")).and(path("/location.name")).and(query_param("accessId", KEY)).and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("location.json")))
        .mount(server).await;
    Mock::given(method("GET")).and(path("/multiDepartureBoard")).and(query_param("accessId", KEY)).and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(board))
        .mount(server).await;
    refuse_others(server).await;
}

/// The fixture's board moved to now: 12:29 becomes the current minute.
fn live_board() -> Value {
    let mut v = fixture("departures.json");
    let base = at(12, 29);
    let now = Utc::now().with_timezone(&TZ).with_second(0).unwrap().with_nanosecond(0).unwrap();
    for d in v["Departure"].as_array_mut().unwrap() {
        for (date, time) in [("date", "time"), ("rtDate", "rtTime")] {
            let Some(t) = d[time].as_str() else { continue };
            let t = NaiveTime::parse_from_str(t, "%H:%M:%S").unwrap();
            let moved = now + (base.date_naive().and_time(t) - base.naive_local());
            d[date] = json!(moved.format("%Y-%m-%d").to_string());
            d[time] = json!(moved.format("%H:%M:%S").to_string());
        }
    }
    v
}

#[tokio::test]
async fn client() {
    let server = MockServer::start().await;
    mock_api(&server, fixture("departures.json")).await;
    let wrong = Client::new(&server.uri(), "wrong");
    assert!(matches!(wrong.search("Nørreport").await, Err(ApiError::Auth(_))));
    let c = Client::new(&server.uri(), &format!(" {KEY} "));
    assert_eq!(c.search("Nørreport").await.unwrap().len(), 2);
    assert!(c.search("xyz").await.unwrap().is_empty());
    let d = c.departures(&[NORREPORT.into(), "8600053".into()], 120).await.unwrap();
    assert_eq!(d.len(), 6);

    let reqs = server.received_requests().await.unwrap();
    let board = reqs.iter().find(|r| r.url.path() == "/multiDepartureBoard").unwrap();
    let q: std::collections::HashMap<_, _> = board.url.query_pairs().into_owned().collect();
    assert_eq!(q["idList"], "8600646|8600053");
    assert_eq!(q["duration"], "120");
}

#[tokio::test]
async fn other_api_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "errorCode": "SVC_LOC", "errorText": "location missing or invalid" })))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri(), KEY);
    match c.departures(&["1".into()], 60).await {
        Err(ApiError::Api { code, .. }) => assert_eq!(code, "SVC_LOC"),
        other => panic!("{other:?}"),
    }
}

fn flow(source: &str, data: Value) -> Value {
    json!({ "flow_id": "f1", "source": source, "data": data })
}

#[tokio::test(flavor = "multi_thread")]
async fn config_flow() {
    let server = MockServer::start().await;
    mock_api(&server, fixture("departures.json")).await;
    let h = host::FakeHost::start(Rejseplanen::default());
    let base = json!({ "base_url": server.uri() });

    let r = h.call("config_flow", json!({ "flow": flow("user", base.clone()), "step": "user", "input": null })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "user");
    assert_eq!(r["result"]["schema"]["properties"]["access_id"]["format"], "password");

    let r = h.call("config_flow", json!({ "flow": flow("user", base.clone()), "step": "user", "input": { "access_id": "wrong" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_auth");

    let r = h.call("config_flow", json!({ "flow": flow("user", base.clone()), "step": "user", "input": { "access_id": KEY } })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "search");
    let data = r["data"].clone();
    assert_eq!(data["access_id"], KEY);

    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "search", "input": { "query": "xyz" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["query"], "no_stops");

    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "search", "input": { "query": "Nørreport" } })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "stop");
    let options = &r["result"]["schema"]["properties"]["stop"]["oneOf"];
    assert_eq!(options[0], json!({ "const": NORREPORT, "title": "Nørreport St. (8600646)" }));
    assert_eq!(options.as_array().unwrap().len(), 2);
    let data = r["data"].clone();

    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "stop", "input": { "stop": NORREPORT } })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "filters");
    assert_eq!(r["unique_id"], NORREPORT);
    let data = r["data"].clone();
    assert!(data.get("_stops").is_none());

    let input = json!({ "kinds": ["metro", "bus"], "lines": "M1, 5C", "directions": "" });
    let r = h.call("config_flow", json!({ "flow": flow("user", data), "step": "filters", "input": input })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], "Nørreport St.");
    let d = &r["result"]["data"];
    assert_eq!(d["stop_id"], NORREPORT);
    assert_eq!(d["access_id"], KEY);
    assert_eq!(d["latitude"], 55.683487);
    assert_eq!(d["filter"], json!({ "lines": "M1, 5C", "directions": "", "kinds": ["metro", "bus"] }));

    // reauth keeps the entry and swaps the key
    let entry = json!({ "entry_id": "e1", "domain": "rejseplanen", "title": "Nørreport St.", "data": { "access_id": "old", "stop_id": NORREPORT }, "options": {} });
    let mut f = flow("reauth", base.clone());
    f["entry"] = entry;
    let r = h.call("config_flow", json!({ "flow": f, "step": "user", "input": { "access_id": KEY } })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["data"], json!({ "access_id": KEY, "stop_id": NORREPORT }));
}

fn entry(id: &str, server: &MockServer, access: &str, stop: &str, options: Value) -> Value {
    json!({
        "entry_id": id, "domain": "rejseplanen", "title": "Nørreport St.",
        "data": { "base_url": server.uri(), "access_id": access, "stop_id": stop, "stop_name": "Nørreport St.", "filter": { "kinds": [] } },
        "options": options,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_and_states() {
    let server = MockServer::start().await;
    mock_api(&server, live_board()).await;
    let h = host::FakeHost::start(Rejseplanen::default());

    // a refused key asks the user to log in again
    let err = h.call("setup_entry", json!({ "entry": entry("bad", &server, "wrong", NORREPORT, json!({})) })).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "auth_failed");

    h.call("setup_entry", json!({ "entry": entry("e1", &server, KEY, NORREPORT, json!({ "kinds": ["metro"] })) })).await.unwrap();
    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.devices[0]["identifiers"], json!([["rejseplanen", NORREPORT]]));
        assert_eq!(rec.devices[0]["entry_type"], "service");
    }
    let next = h.entity_ending("ste_afgang");
    let minutes = h.entity_ending("minutter_til_afgang");
    let (spec, _) = h.rec.lock().unwrap().entities[&minutes].clone();
    assert_eq!(spec["device_class"], "duration");
    assert_eq!(spec["unit_of_measurement"], "min");

    let mut tries = 0;
    while h.state(&minutes).is_none() && tries < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        tries += 1;
    }
    // the options win over the setup's filter: metro only, M3 cancelled
    let (state, attrs) = h.state(&next).expect("no state");
    assert_eq!(attrs["line"], "M1");
    let t = DateTime::parse_from_rfc3339(&state).unwrap();
    let left = t.with_timezone(&Utc) - Utc::now();
    assert!(left > Duration::minutes(3) && left <= Duration::minutes(4), "{left}");
    let (m, _) = h.state(&minutes).unwrap();
    assert_eq!(m, "3");

    // a second stop with the same key joins the poller, which then asks for both
    h.call("setup_entry", json!({ "entry": entry("e2", &server, KEY, "8600053", json!({})) })).await.unwrap();
    let mut both = false;
    for _ in 0..50 {
        let reqs = server.received_requests().await.unwrap();
        both = reqs.iter().any(|r| r.url.query_pairs().any(|(k, v)| k == "idList" && v == "8600053|8600646"));
        if both {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(both, "no call for both stops");

    let diag = h.call("diagnostics", json!({ "entry": entry("e1", &server, KEY, NORREPORT, json!({})) })).await.unwrap();
    assert_eq!(diag["entry"]["access_id"], "**REDACTED**");
    assert_eq!(diag["board"]["stops"], json!(["8600053", NORREPORT]));

    h.call("unload_entry", json!({ "entry_id": "e1" })).await.unwrap();
    h.call("unload_entry", json!({ "entry_id": "e2" })).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn options_flow() {
    let server = MockServer::start().await;
    let h = host::FakeHost::start(Rejseplanen::default());
    let mut f = flow("options", json!({}));
    f["entry"] = entry("e1", &server, KEY, NORREPORT, json!({ "lines": "A", "directions": "", "kinds": ["s_tog"] }));
    let r = h.call("options_flow", json!({ "flow": f.clone(), "step": "options", "input": null })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "filters");
    let props = &r["result"]["schema"]["properties"];
    assert_eq!(props["lines"]["default"], "A");
    assert_eq!(props["kinds"]["default"], json!(["s_tog"]));
    let r = h.call("options_flow", json!({ "flow": f, "step": "filters", "input": { "lines": "B, C", "kinds": [] } })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["data"], json!({ "lines": "B, C", "directions": "", "kinds": [] }));
}
