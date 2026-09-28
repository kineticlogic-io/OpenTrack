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

- **Bearing.** `position` is the sensor's position, where the line starts, so an old consumer never mistakes it for the object's. **Every bearing must carry its platform's position at the bearing's time, in the same message.** OpenTrack doesn't take a platform's position from another feed (a navigation or ownship feed) or from a track, and doesn't interpolate. A bearing without a position is refused like any observation. A sensor whose bearings and navigation come separately needs them merged before OpenTrack, and for a moving platform that merge must be to the bearing's time: at 110 m/s, 5 s of staleness is 550 m. The mapping fills the rest from the feed's own fields:
  - `bearing_deg` is the direction, in degrees true;
  - `sigma_deg` is one standard deviation;
  - `max_range_m` is how far the sensor could plausibly detect, if known;
  - `elevation_deg` is optional, for air.
- **Area.** `position` is the area's centre and `uncertainty` its covering ellipse, so everything that handles points still works. A polygon adds the exact shape, for display and for containment tests. An area given only as an ellipse needs no `geometry`: a point with a large ellipse *is* an area. What changes is how correlation treats one, below.
- **Emitter parameters** are identifiers and attributes that already exist: the `elnot` identifier scheme, and admin-defined output schema fields for RF, PRI and so on. Nothing new in the model. What changes is that the engine uses them to decide which bearings belong together.
- **An ELNOT is evidence, not identity.** It names a kind of emitter: every boat with the same radar model shares one, and one platform can carry several. So an ELNOT:
  - never pairs tracks on its own, and never keys a track;
  - adds 10:1 for the same object when two reports share one, in correlation and in bearing association;
  - counts for nothing either way when they differ, since a platform can carry several radars.

  (`EVIDENCE_SCHEMES` in `correlate.rs`; other identifier schemes, such as an MMSI, stay identities.)

## Association: which track does it belong to?

**A bearing and a track.** Predict the track to the bearing's time, and take the bearing from the sensor to it. The residual (measured minus predicted, in degrees) is compared against:
- the bearing's own error;
- the track's position error seen from the sensor (its error across the line, divided by its range).

The comparison is a chi-square gate with one degree of freedom. A track beyond `max_range_m`, or behind the sensor, is out.

A bearing then **reports for** a track, the way a detection's plot does, in either of two cases:
- **Its ELNOT matches one the track carries,** and the track fits clearly better than any other: a likelihood ratio above 10, the ELNOT counting 10:1. A different ELNOT counts for nothing.
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
   - **Identity first.** Bearings carrying an ELNOT are only combined with bearings of the same ELNOT. Two of them, from two sensors, may fix, but only by repeating like an anonymous set (below): two boats with the same radar model share an ELNOT.
   - **Otherwise, consensus.** Without identity, a fix needs **three or more sensors** agreeing:
     - the set passes a chi-square test at 99%;
     - each line agrees with the fix of the others (so a line from another emitter passing close by can't hide by dragging the fix towards itself);
     - it doesn't split sensor tracks whose emitters already fixed apart.
   - **Twice.** Every set, ELNOT or anonymous, fixes only the second time the same sensor tracks cross where the first crossing could have moved to (40 m/s). Three unrelated lines meeting once is chance; twice is not.
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
   - a moving target's manoeuvre over the window;
   - for a fixed fit, how far the emitter may have moved unseen (motion along the line of sight barely turns the bearings), at the fastest speed the bearings allow;
   - 2% of the range.

   The manoeuvre and the top speed are the source's `emitter_motion` setting (Sources → the source → Publish), since they depend on what it listens for. The default is surface traffic, 0.1 m/s² and 30 m/s. A small fast boat weaves at 0.2-0.3 m/s², and an aircraft turns at several.

   ```json
   "emitter_motion": { "manoeuvre_mps2": 0.25, "max_speed_mps": 25 }
   ```

   Over 60 noise draws per case (a fixed radar from a straight leg and from a dogleg, and a 20 m/s boat), the truth falls inside the stated 2σ ellipse at least 90% of the time.
4. **When it publishes.** Only once the platform has moved at least 1 km over the window (a fixed sensor never self-locates), and while the error is under a quarter of the range and under 20 km. It falls quiet once its own location has joined a track that another, better-located sensor (video, AIS) also reports for. Its bearings keep reporting for that track. While the bearings only go to a separate track (ELINT's, sharing the ELNOT), it keeps reporting, so the two can pair and the track keeps its number from first contact.
5. **Into the picture.** A location is a report of the `fix` source, keyed by the sensor track (`tma:<source>/<key>`). Its ELNOT rides along as evidence, so ELINT of the same ELNOT pairs with it on position plus the 10:1 ELNOT evidence, never on the ELNOT alone.

Scenario `esm-patrol` (`esm_patrol_intercept_converges` in `crates/ot-server/src/engine.rs`):
- A patrol aircraft carries ESM (2°, `emitter_motion` for small boats: 0.25 m/s², 25 m/s), ELINT (an ellipse every 20 s, once it has held the emitter two minutes) and FMV (a video tracker's track inside 10 km). It flies a racetrack.
- A 40-knot boat with no AIS runs a weaving course. The distractors are three silent AIS fishing boats and an AIS cargo ship whose own radar the ESM also hears.
- On first contact the aircraft flies a three-minute leg across the bearing. Once OpenTrack holds a confirmed track, it intercepts and orbits at 3 km, steering on the best-located track it has.

| Measure | Result |
|---|---|
| ESM first hears the boat | 225 s |
| A track from the ESM alone | 285 s |
| ELINT joins that track | 460 s, 100 s after its first report (position plus ELNOT evidence) |
| Video joins it | 810 s, 15 s after the video starts |
| The boat's bearings on its track | 402 of 423 |
| The cargo ship's bearings on the boat's track | 0 (its own ESM location pairs with its AIS track) |
| Tracks at the boat at the end | 1: ESM location, ELINT and video, 20 m from the truth, one track number throughout |

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
| Bearings associated with the right track | 98.9% (2,271 of 2,297) |
| Emitters without AIS that got a track | 10 of 10; 9 within 60 s |
| Ghost fixes (no emitter within 3σ) | 0.44% (3 of 683) |
| Fixes within 2σ of their own ellipse | 92% |
| Ships with two tracks at the end | 3 of 20 (gate: 3 or fewer) |

Since the ELNOT became evidence (2026-09-28), an ELNOT emitter's fixes and its ELINT areas no longer meet on identity. They pair on position plus the ELNOT, which ELINT's kilometre-wide areas make slow. Three ELINT ships now end with a fix track beside their ELINT track (before: one, with 2,769 bearings on tracks).

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
