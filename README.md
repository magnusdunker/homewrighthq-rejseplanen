# Rejseplanen til HomewrightHQ

En ekstern integration til [HomewrightHQ](https://github.com/magnusdunker/homewrighthq), der viser de næste afgange fra et stoppested: bus, tog, S-tog, metro, letbane og færge. Den henter afgangstavlen fra Rejseplanens API 2.0, med forsinkelser og aflysninger i realtid.

## Adgangsnøgle

Rejseplanens API kræver en adgangsnøgle (`accessId`). Den er gratis for private:

1. Gå til [Rejseplanen Labs](https://labs.rejseplanen.dk/) og find formularen, hvor man søger om adgang til API 2.0.
2. Vælg, at du er privat bruger, og skriv kort, hvad nøglen skal bruges til, f.eks. "afgangstavle i mit hjem".
3. Nøglen kommer på mail. Det kan tage et par hverdage.

En privat nøgle giver 50.000 kald om måneden. Integrationen spørger én gang i minuttet og hvert 5. minut mellem kl. 01 og 05. Det er omkring 39.000 kald om måneden, og det samme, uanset hvor mange stoppesteder du tilføjer med samme nøgle, for de hentes alle i ét kald.

Det gamle API på `xmlopen.rejseplanen.dk` blev lukket i december 2024 og svarer ikke længere.

## Installation

I HomewrightHQ: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/homewrighthq-rejseplanen
```

og tryk **Installér**. Tilføj derefter **Rejseplanen** under **Tilføj integration**:

1. Indsæt adgangsnøglen. Den prøves med det samme.
2. Søg efter stoppestedet med navnet eller en del af det, f.eks. `Aarhus H` eller `Nørreport`.
3. Vælg stoppestedet på listen. Samme navn kan høre til flere stoppesteder, f.eks. et for toget og et for togbussen, så nummeret står ved siden af.
4. Vælg eventuelt, hvilke afgange der skal med.

Hvert stoppested er en opsætning for sig. Tilføj integrationen igen for det næste.

### Filtre

- **Transportmidler**: bus, tog (IC, Lyn og regionaltog), S-tog, metro, letbane og færge. Ingen valgt viser alle.
- **Linjer**: f.eks. `2A, 18` eller `A, B`. Store og små bogstaver er lige gode.
- **Retning**: tekst, som retningen skal indeholde, f.eks. `Skejby` eller `Hillerød`. Flere adskilles med komma.

Filtrene kan ændres senere under integrationens indstillinger.

## Hvad du får

Hvert stoppested bliver en enhed i HomewrightHQ med to sensorer:

- **Næste afgang**: tidspunktet for den næste afgang, der ikke er aflyst, med realtid, hvis Rejseplanen har det.
- **Minutter til afgang**: antal hele minutter til den samme afgang.

Står der ingen afgange inden for de næste to timer, er tilstanden `unknown`.

**Næste afgang** har attributterne `line`, `name`, `type`, `direction`, `track`, `planned` og `delay` (minutter) for den næste afgang, og `departures` med de næste 10 afgange. Aflyste afgange er med i listen med `cancelled: true`:

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

**Minutter til afgang** har `line`, `direction`, `track` og `delay`.

Et stoppested som Nørreport St. har både perroner og busstoppesteder under sig. Deres afgange kommer med, når hovedstoppestedet er valgt.

Afviser Rejseplanen nøglen, kommer der en besked under **Reparationer**, og integrationen beder om en ny nøgle. Svarer Rejseplanen ikke, vises de afgange, der blev hentet sidst, i op til 30 minutter. Derefter bliver sensorerne utilgængelige.

## Udvikling

Bygget i Rust på SDK'en `thehome-plugin` fra [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: Rejseplanens API, `location.name` og `multiDepartureBoard` i JSON
- `src/board.rs`: filtrene og fra afgangstavlen til sensorernes tilstand
- `src/lib.rs`: opsætningen, den fælles afhentning pr. nøgle og forbindelsen til HomewrightHQ
- `tests/`: tests mod et falsk API med svar i Rejseplanens format i `tests/fixtures` og en falsk HomewrightHQ i `tests/host`

```
cargo test
cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.2.0 && git push origin v0.2.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne og `homewright.json` på releasen.

## Licens

MIT
