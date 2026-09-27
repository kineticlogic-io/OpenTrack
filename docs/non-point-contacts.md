# Non-point contacts: design

Status: **agreed scope** (2026-09-27); building.

## What this adds

Every contact in OpenTrack is a point today: a position, and perhaps an error ellipse. Some sensors don't give a point:

| Contact | What the sensor says | Examples |
|---|---|---|
| **Line of bearing (LOB)** | From here, the object lies in this direction, give or take so many degrees. No range. | Direction finding, ESM, passive sonar |
| **Area of uncertainty (AOU)** | The object is somewhere inside this ellipse or polygon; the centre is not a good position. | An ELINT emitter location, a sonobuoy field |
| **Emitter parameters** | What the emitter is, not where. | ELNOT, frequency, PRI, pulse width, scan |

Decided scope: all three, **fused into tracks**. OpenTrack:
- keeps and publishes them;
- associates each with the track it belongs to;
- cross-fixes bearings from several sensors into positions that start or update tracks.

## The model

An `Observation` gains one optional field, `geometry`:

```json
"geometry": { "type": "bearing", "bearing_deg": 047.5, "sigma_deg": 2.0,
              "max_range_m": 60000, "origin": {"latitude": 50.6, "longitude": -1.9} }
"geometry": { "type": "area", "polygon": [[50.70, -1.30], [50.72, -1.20], [50.65, -1.18]] }
```

- **Bearing.** `position` is the sensor's position, where the line starts, so an old consumer never mistakes it for the object's. The mapping fills it from the feed's own fields:
  - `bearing_deg` is the direction, in degrees true;
  - `sigma_deg` is one standard deviation;
  - `max_range_m` is how far the sensor could plausibly detect, if known;
  - `elevation_deg` is optional, for air.
- **Area.** `position` is the area's centre and `uncertainty` its covering ellipse, so everything that handles points still works. A polygon adds the exact shape, for display and for containment tests. An area given only as an ellipse needs no `geometry`: a point with a large ellipse *is* an area. What changes is how correlation treats one, below.
- **Emitter parameters** are identifiers and attributes that already exist: the `elnot` identifier scheme, and admin-defined output schema fields for RF, PRI and so on. Nothing new in the model. What changes is that the engine uses them to decide which bearings belong together.

## Association: which track does it belong to?

**A bearing and a track.** Predict the track to the bearing's time, and take the bearing from the sensor to it. The residual (measured minus predicted, in degrees) is compared against:
- the bearing's own error;
- the track's position error seen from the sensor (its error across the line, divided by its range).

The comparison is a chi-square gate with one degree of freedom. A track beyond `max_range_m`, or behind the sensor, is out.

A bearing then **reports for** a track, the way a detection's plot does, if both hold:
- **Only one track passes the gate.** Or the best passes clearly better than the next: a likelihood ratio above a threshold, default 10.
- **Its emitter identity doesn't conflict with the track's.** An ELNOT the track already carries from a different emitter is a veto, and a matching one is strong evidence.

Two consequences:
- **It never moves the track.** A bearing contributes identity (the emitter's ELNOT and parameters), confidence that the track exists, and evidence for the track card. A track seen only by bearings gets its position from cross-fixes, below.
- **An ambiguous bearing stays unassociated** and goes to cross-fixing. Ships in a harbour all sit on the same line, so this case is common.

**An area and a track.** The existing kinematic gate already copes with a large ellipse. The addition is the **same ambiguity rule**: an area containing several tracks pairs with none of them until one clearly fits best. That happens as the tracks move and the areas are updated. A polygon also has to contain the track's predicted position. An area that pairs with nothing becomes a track of its own, published with its area.

## Cross-fixing: positions from bearings

The engine keeps the bearings no track took, for a window: by default 60 s, and never longer than an object could move across the fix's error.

It looks for bearings **from different sensor positions** that agree on a point:
1. **Candidate sets.** Pairs whose lines cross at more than 20° (flatter crossings give a long, useless error ellipse), within `max_range_m` of both sensors, in front of both.
2. **The fix.** A least-squares intersection of the set, weighted by each bearing's error, gives a position and a covariance. The covariance comes from the geometry, so a narrow crossing angle shows as a long ellipse. Each member's residual must pass its gate.
3. **Ghosts.** With several emitters, pairs of bearings also cross where nothing is: the classic ghost problem. OpenTrack doesn't publish ghosts:
   - **Identity first.** Bearings carrying an emitter identity (an ELNOT, or parameters close enough) are only combined with each other. Two bearings of the same ELNOT from two sensors make a real fix.
   - **Otherwise, consensus.** Without identity, a fix needs **three or more sensors** agreeing within their gates. A ghost needs three unrelated lines to cross at one point, which happens rarely.
   - **Otherwise, persistence.** A two-sensor fix without identity stays tentative. It is published only if later fixes agree, consistent with a moving object.
4. **Into the picture.** A fix becomes an observation of the built-in source `fix`, with its position, its error ellipse and the identity of its bearings. From there, normal correlation takes it:
   - it updates the track it pairs with;
   - otherwise it starts a track, tentative until confirmed like any sensor's (default 3 reports).

   The bearings it came from are marked used, so they don't also make other fixes.

Bearings-only target motion analysis (fixing a moving target from one sensor's bearings over time) isn't in this phase. It's noted for later.

## What is published

- **Tracks carry their non-point evidence.** A track message gains two optional fields:
  - `area`: the polygon, or the ellipse, when the track's position comes from an area;
  - `bearings`: the latest bearing from each sensor reporting for it, with origin, direction and error, so a consumer can draw the lines.
- **Bearings no track took** go on their own subject, `contacts.bearing.<source>.<key>`, in a separate stream (`CONTACTS`, latest per subject, aged out after 10 minutes). This keeps consumers of `tracks.>` unaffected.
- **Fixes** are tracks like any other; their `provenance` says they came from cross-fixing and from which sensors.

## Display

- **The map:**
  - a bearing is a line from its sensor out to its range;
  - an area is its polygon or ellipse;
  - the lines through a fix meet at its track.
- **The track card** lists the bearings and areas that report for the track, with each residual and the emitter identity.

## How we will know it works

A synthetic scenario, `esm-crossfix`:
- **Emitters:** ships and aircraft carrying radars, each with an ELNOT. Some ships also report AIS.
- **Sensors:** three to five ESM sensors, fixed and moving, reporting bearings with realistic errors (1–3°), some with the ELNOT and some without.
- **An ELINT source** reports areas.

| Measure | Gate |
|---|---|
| Bearings associated with the right track (precision) | ≥ 95% |
| Emitters with a track within 60 s of three sensors seeing them | ≥ 90% |
| Fix error | within 2σ of its own ellipse, 90% of the time |
| Ghost tracks published (no emitter within 3σ) | < 2% of published fix tracks |
| Areas paired with the right track | ≥ 95%, and never with several |

## Build order

1. **Model.** `geometry` on observations, validated. Mapping destinations for bearings and polygons. The engine routes bearings and areas to their own paths.
2. **Association.** The bearing-to-track gate, the ambiguity rule for bearings and areas, and emitter identity as veto and evidence.
3. **Cross-fixing.** The least-squares fix with covariance, the three ghost rules, and the `fix` source.
4. **Output.** `area` and `bearings` on tracks, the `CONTACTS` stream, and `docs/nats-output.md`.
5. **UI.** Lines and areas on the map, and the track card.
6. **Benchmark.** The `esm-crossfix` scenario and the gates.
