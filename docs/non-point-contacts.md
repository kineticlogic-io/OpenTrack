# Non-point contacts: design

Status: **built** (2026-09-27), all six steps, plus single-sensor location (2026-09-28); both scenarios meet their gates (below).

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

A bearing then **reports for** a track, the way a detection's plot does, in either of two cases:
- **Its emitter identity matches the track's,** and the track fits clearly better than any other: a likelihood ratio above 10. An ELNOT the track already carries from a different emitter is a veto.
- **Its emitter was found on the track.** Either:
  - lines from three or more sensors, fixed together, all point at the track's own position within their errors and the track's. The track must be clearly the likeliest of all the tracks they could point at, of any kind: their fix is weighed against each track's position with both errors, and the best must beat the next by 10. A kilometres-wide ELINT area fits loosely and so scores low, while a precise AIS ship the lines pass right through scores high. Only a track at least as precise as the lines can bind them, and two lines always meet somewhere, so two aren't enough;
  - or the sensor track's earlier bearings went into a cross-fix that now reports for this track.

  From then on, the sensor track's bearings go to it directly while they pass its gate. A line that stops fitting (a neighbour that passed close by and moved on) is unbound.

One anonymous line through a track is **not** enough. An emitter with no track of its own often lies on a line through someone else's, so this rule attached a third of such lines to the wrong ship in the benchmark. Instead the line waits and is cross-fixed.

Testing the lines against the track, not the fix's position against it, is what makes this work. A fix is typically a kilometre uncertain, so comparing its position with a ship's gives evidence too weak to pair them; the lines, measured against the ship's precise position, give strong evidence.

**Object density.** A fix pairs with other tracks by normal correlation, which weighs a match against "another object nearby" at `kinematic.object_density_per_km2`. The default, 1 per km², suits a harbour. There, a fix with a kilometre of error is weaker evidence than chance and never pairs. In open water, set it to the real density: the benchmark uses 0.02.

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
   - **Otherwise, consensus.** Without identity, a fix needs **three or more sensors** agreeing:
     - the set passes a chi-square test at 99%;
     - each line agrees with the fix of the others (so a line from another emitter passing close by can't hide by dragging the fix towards itself);
     - it doesn't split sensor tracks whose emitters already fixed apart.
   - **Twice.** An anonymous set fixes only the second time the same sensor tracks cross where the first crossing could have moved to (40 m/s). Three unrelated lines meeting once is chance; twice is not.
   - **Not already explained.** A set whose every line passes through some other track is rejected: each line is accounted for, and their crossing is a ghost of those tracks' lines.
   - **Otherwise, no fix.** Two anonymous lines are the ghost case itself, so they wait for a third sensor, or for the window to pass.

   Sets are chosen **once a moment's lines are all in**, not as each line arrives: sets that fit well (chi-square at 95%) come first, the most sensors first among them. Choosing on arrival let a line join a set before the emitter's own lines from the other sensors were in.
4. **Into the picture.** A fix becomes an observation of the built-in source `fix`, with its position, its error ellipse and the identity of its bearings. From there, normal correlation takes it:
   - it updates the track it pairs with;
   - otherwise it starts a track, tentative until confirmed like any sensor's (default 3 reports).

   The bearings it came from are marked used, so they don't also make other fixes.

## Single-sensor location

The primary use is **one sensor**: an aircraft or ship carrying its own ESM, locating what it hears from its own motion. A sensor that moves sees the emitter from a changing place, so its own track is the baseline. OpenTrack keeps each ESM sensor track's recent bearings (one emitter, as the sensor's own tracker keeps it) and fits them by weighted least squares (`crates/ot-server/src/engine/tma.rs`):

1. **Fixed or moving.** It first fits a **fixed** emitter (position only). It switches to a **moving** one (position and constant velocity: bearings-only target motion analysis) when:
   - the bearings stop fitting a fixed point (chi-square at 99%);
   - or a moving target fits them clearly better (a likelihood-ratio test at 99%).

   An emitter once seen moving stays on the moving model.
2. **The window** shrinks with range: 5 minutes far out, 1 minute close in, since a target's manoeuvres matter more the closer it is.
3. **An honest error.** The covariance comes from the geometry: a straight leg gives a long thin ellipse along the line, and a turn across it tightens the fix. Three things the model can't see are added to it:
   - a moving target's manoeuvre, up to 0.1 m/s² over the window;
   - for a fixed fit, how far the emitter may have moved unseen (motion along the line of sight barely turns the bearings), at the fastest speed the bearings allow, up to 30 m/s;
   - 2% of the range.

   Over 60 noise draws per case (a fixed radar from a straight leg and from a dogleg, and a 20 m/s boat), the truth falls inside the stated 2σ ellipse at least 90% of the time.
4. **When it publishes.** Only once the platform has moved at least 1 km over the window (a fixed sensor never self-locates) and the error is under a quarter of the range and under 20 km. And only while no better-located track holds the emitter: once the sensor track's bearings go to a track located better (video, AIS), its own location falls quiet, and its bearings keep reporting for that track.
5. **Into the picture.** A location is a report of the `fix` source, keyed by the emitter's identity when the bearings carry one (`id:elnot:…`), else by the sensor track. So ELINT or another fix of the same ELNOT meets it on identity.

Scenario `esm-patrol` (`esm_patrol_intercept_converges` in `crates/ot-server/src/engine.rs`):
- A patrol aircraft carries ESM (2°), ELINT (an ellipse every 20 s, once it has held the emitter two minutes) and FMV (a video tracker's track inside 10 km). It flies a racetrack.
- A 40-knot boat with no AIS runs a weaving course. The distractors are three silent AIS fishing boats and an AIS cargo ship whose own radar the ESM also hears.
- On first contact the aircraft flies a three-minute leg across the bearing. Once OpenTrack holds a confirmed track, it intercepts and orbits at 3 km, steering on the best-located track it has.

| Measure | Result |
|---|---|
| ESM first hears the boat | 225 s |
| A track from the ESM alone | 285 s |
| ELINT joins that track | 360 s, on its first report |
| Video joins it | 780 s, 20 s after the video starts |
| The boat's bearings on its track | 417 of 423 |
| The cargo ship's bearings on the boat's track | 0 (its own ESM location pairs with its AIS track) |
| Tracks at the boat at the end | 1: ESM location, ELINT and video, 22 m from the truth |

## What is published

- **Tracks carry their non-point evidence.** A track message gains two optional fields:
  - `area`: the polygon, or the ellipse, when the track's position comes from an area;
  - `bearings`: the latest bearing from each sensor reporting for it, with origin, direction and error, so a consumer can draw the lines.
- **Bearings no track took** go on their own subject, `contacts.bearing.<source>.<key>`. They are published live on core NATS and not stored, because they go stale within a minute. This keeps consumers of `tracks.>` unaffected.
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

**Results** (`esm_crossfix_scenario_meets_its_gates` in `crates/ot-server/src/engine.rs`): 20 ships in a 50 km box, 10 on AIS; 4 ESM sensors at 1.5°, half the emitters with an ELNOT; an ELINT source with 3 km areas for 5 of them; 10 minutes.

| Measure | Result |
|---|---|
| Bearings associated with the right track | 99% (2,547 of 2,573) |
| Emitters without AIS that got a track | 10 of 10; 9 within 60 s |
| Ghost fixes (no emitter within 3σ) | 0.46% (3 of 656) |
| Fixes within 2σ of their own ellipse | 92% |
| Ships with two tracks at the end | 2 of 20 (gate: 3 or fewer) |

The hard cases that remain:
- **An emitter only two sensors see, beside its own AIS ship.** Two lines can't bind, and its fix is too uncertain to pair by position. That leaves a duplicate for an ELNOT emitter, and unattached lines for an anonymous one. Linking an ELNOT to an MMSI on the entity would join the first kind.
- **Two ships in line with two sensors' baseline.** Those sensors' lines to them coincide, so their sets mix and neither binds; the 26 wrong bearings are of this kind too.
- **A ship known otherwise only by a kilometres-wide ELINT area** can end with a fix track beside it. Two of the emitters without AIS had no track at the end, having gone out of three sensors' range.

The videos render from the same run: `OT_REPLAY_TRACE=<dir>` writes a frame per step, and `scripts/benchmark/replay/esm-video.py` draws them. `OT_ESM_NAIVE=1` turns the ghost rules off, for comparison.

Area pairing is covered by unit tests, not yet scored by the scenario.

## Build order

1. **Model.** `geometry` on observations, validated. Mapping destinations for bearings and polygons. The engine routes bearings and areas to their own paths.
2. **Association.** The bearing-to-track gate, the ambiguity rule for bearings and areas, and emitter identity as veto and evidence.
3. **Cross-fixing.** The least-squares fix with covariance, the three ghost rules, and the `fix` source.
4. **Output.** `area` and `bearings` on tracks, the `CONTACTS` stream, and `docs/nats-output.md`.
5. **UI.** Lines and areas on the map, and the track card.
6. **Benchmark.** The `esm-crossfix` scenario and the gates.
