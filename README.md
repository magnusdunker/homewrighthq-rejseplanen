# Rejseplanen for HomewrightHQ

An external integration for [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) that shows the next departures from a stop: bus, train, S-train, metro, light rail and ferry. It fetches the departure board from Rejseplanen's API 2.0, with real-time delays and cancellations.

## Access key

Rejseplanen's API requires an access key (`accessId`). It is free for private users:

1. Go to [Rejseplanen Labs](https://labs.rejseplanen.dk/) and find the form where you apply for access to API 2.0.
2. Choose that you are a private user, and write briefly what the key will be used for, e.g. "afgangstavle i mit hjem" (departure board in my home).
3. The key comes by email. It can take a few working days.

A private key gives 50,000 calls a month. The integration polls once a minute, and every 5 minutes between 01:00 and 05:00. That is about 39,000 calls a month, and the same no matter how many stops you add with the same key, because they are all fetched in one call.

The old API at `xmlopen.rejseplanen.dk` was shut down in December 2024 and no longer responds.

## Installation

In HomewrightHQ: **Settings → Integrations → From GitHub**. Paste

```
https://github.com/magnusdunker/homewrighthq-rejseplanen
```

and press **Install**. Then add **Rejseplanen** under **Add integration**:

1. Paste the access key. It is tested straight away.
2. Search for the stop by its name or part of it, e.g. `Aarhus H` or `Nørreport`.
3. Choose the stop from the list. The same name can belong to several stops, e.g. one for the train and one for the rail replacement bus, so the number is shown next to it.
4. Optionally choose which departures to include.

Each stop is its own entry. Add the integration again for the next one.

### Filters

- **Transportmidler** (means of transport): bus, train (IC, Lyn and regional trains), S-train, metro, light rail and ferry. None selected shows all.
- **Linjer** (lines): e.g. `2A, 18` or `A, B`. Upper and lower case are treated the same.
- **Retning** (direction): text the direction must contain, e.g. `Skejby` or `Hillerød`. Separate several with commas.

The filters can be changed later under the integration's options.

## What you get

Each stop becomes a device in HomewrightHQ with two sensors:

- **Næste afgang** (next departure): the time of the next departure that is not cancelled, with real time if Rejseplanen has it.
- **Minutter til afgang** (minutes to departure): the number of whole minutes to the same departure.

If there are no departures within the next two hours, the state is `unknown`.

**Næste afgang** has the attributes `line`, `name`, `type`, `direction`, `track`, `planned` and `delay` (minutes) for the next departure, and `departures` with the next 10 departures. Cancelled departures are in the list with `cancelled: true`:

```yaml
departures:
  - line: M1
    name: Metro M1
    type: MET
    direction: Vanløse St.
    planned: "2026-10-18T12:33:00+02:00"
    realtime: null
    delay: null
    track: null
    cancelled: false
    part_cancelled: false
```

**Minutter til afgang** has `line`, `direction`, `track` and `delay`.

A stop such as Nørreport St. has both platforms and bus stops under it. Their departures are included when the main stop is selected.

If Rejseplanen rejects the key, a message appears under **Repairs**, and the integration asks for a new key. If Rejseplanen does not respond, the departures fetched last are shown for up to 30 minutes. After that the sensors become unavailable.

## Development

Built in Rust on the `thehome-plugin` SDK from [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: Rejseplanen's API, `location.name` and `multiDepartureBoard` in JSON
- `src/board.rs`: the filters, and from the departure board to the sensors' state
- `src/lib.rs`: the setup, the shared fetch per key and the connection to HomewrightHQ
- `tests/`: tests against a fake API with responses in Rejseplanen's format in `tests/fixtures` and a fake HomewrightHQ in `tests/host`

```
cargo test
cargo build --release
```

A new version is released with a tag, e.g. `git tag v0.2.0 && git push origin v0.2.0`. The workflow builds for Windows, Linux (x86_64 and arm64, static musl) and macOS and puts the binaries and `homewright.json` on the release.

## License

MIT
