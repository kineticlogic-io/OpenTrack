# Algorithm versions

OpenTrack versions the behaviour of its correlation engine and of each tracker
separately from the release version, because they are what change while we tune
against recorded data. A version names *what the algorithm does*: bump it whenever
the same inputs would give different output, add an entry below, and record its
scores on the benchmark.

| Component | Constant | Stamped on |
|-----------|----------|------------|
| Correlation engine: pairing and merging, best source, detection association, lifecycle, publish rule | `correlate::VERSION` (ot-server) | every engine decision in the track graph (`evidence.correlation_version`), every published message (`publisher.correlation`), `/api/v1/status` |
| GNN tracker | `tracker::GNN_VERSION` (ot-source) | every track report it makes (`provenance.tracker`), kinematic merge evidence (`trackers`), `/api/v1/status` |
| MHT tracker | `tracker::MHT_VERSION` (ot-source) | same as GNN |

A test (`algorithm_versions_are_documented`) fails if a running version has no entry
here, so a bump cannot ship without its notes.

## Benchmark

Autoferry sensor fusion dataset (NTNU, CC0), scenarios 2 (crossing) and 16 (longest
range), replayed by the tests in `crates/ot-source/src/tracker/tests.rs` and
`crates/ot-server/src/engine.rs` (`replay_autoferry_*`). Run them with
`OT_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test autoferry -- --nocapture`. The wider benchmark
(`scripts/benchmark/`, GOSPA and SIAP-style scores over nine Autoferry scenarios, GMTI, AIS and
ADS-B with simulated radars) is how parameter and algorithm changes are compared.

- **Tracker**: per vessel, the reports and purity of its best track; tracks that mostly
  follow clutter.
- **Correlation, sensor tracks**: lidar and radar tracks (from the tracker, or from the
  fixtures' own simple tracker) paired with the vessel track feed: right, wrong,
  unpaired, and clutter tracks paired with a vessel.
- **Correlation, raw detections**: plots associated with the right vessel's track, wrong,
  missed (mostly a second plot on a vessel in the same scan), clutter associated.

### GMTI and tracker auto timing

A tracker's `auto_timing` sets its windows from the sensor's revisit period, which the
source's codec measures from the stream (STANAG 4607: the time between the first dwells
of consecutive revisits of a job, a running median; the job's nominal revisit until one
revisit is complete). Each window is a multiple of the period, never below a floor:

| Window | Multiple | Floor |
|--------|----------|-------|
| drop an unconfirmed track without a plot | 1.3 revisits | 8 s |
| drop a confirmed track without a plot | 2.5 revisits | 30 s |

With the revisit known, a track's existence (gnn-2, mht-2) counts a miss once per revisit
without a plot, not once per scan: a GMTI scan is one dwell, which may not have looked at
the track. (Before gnn-2 auto timing also set the confirmation window, 3.5 revisits and at
least 20 s; confirmation is now by existence.)

The floors cover what the revisit does not: a ground mover drops out of GMTI for longer
than a fast revisit (terrain, stopping below the minimum detectable velocity). The
windows follow a new job's period; changes within 5% are ignored. It is a setting, not a
new tracker version: the same windows give the same output as before.

Scored on real GMTI recordings (not in the repository; `$OT_GMTI_SAMPLES`), by the share
of GNN tracks that last 30 s or more (fewer, longer tracks mean less fragmentation):

| Recording | Revisit (measured; nominal says 6 s) | Pure multiples | Auto timing (with floors) |
|-----------|---------|----------------|---------------------------|
| Stuart Hwy 48 | 2.56 s (7 dwells) | 39 of 100 | 79 of 85 |
| Stuart Hwy 94 | 2.56 s | 26 of 88 | 98 of 107 |
| Stuart Hwy 115 | 12.8 s (25 dwells) | 25 of 29 | 25 of 29 |

`crates/ot-source/tests/stanag4607.rs` runs the example source over the first and last.
With gnn-2 (existence; the example's detection probability 0.5, clutter 3·10⁻⁸ /m², birth
3·10⁻⁹ /m², and a ground error ellipse per detection from the radar geometry): 72 of 79,
83 of 93 and 19 of 22.

## Changelog

### correlation-3 (2026-09-25)

Proper error propagation and a probability for every pairing:

- **Comparison.** Each report becomes a state with a covariance: its own full covariance
  (a tracker's), else its ellipse or circular error (floored at `min_sigma_m`), and its
  course and speed with `speed_sigma_mps` (a speed within that and no course, as a
  stopped GPS reports, is a velocity of zero; a domain-wide spread when it reports none).
  The older report is propagated to the newer one's time (constant velocity,
  white-acceleration noise `process_noise_mps2`) and their difference tested against the
  sum of both covariances: position and velocity (4 degrees of freedom) when both report
  motion, else position. The two are taken as independent (separate sensors); the common
  process noise of two tracks of one object is not modelled. Replaces the circular σ and
  flat drift of correlation-2, which never compared velocity.
- **Evidence.** A comparison is a likelihood ratio: the Gaussian density of the difference
  against another object being there (`object_density_per_km2`, and
  `velocity_spread_mps` for velocity). Outside the gate (`gate_probability`, 0.99) it
  counts at most (1 − 0.99) : 1. A comparison counts only when both sides bring a report
  newer than the one each gave the last counted comparison (new reports against one side's
  same old report are not new evidence), at least `min_interval_secs` apart (consecutive
  reports are not independent either); the last `n` within the window count.
- **Pairing.** The probability of the same object is `prior_probability` (0.01) updated
  by that evidence; pair (or suggest) at `pair_probability` (0.99) after at least `m`
  comparisons. Replaces m of n passing a chi-square gate.
- **Confidence.** A paired source track's probability of being the same object starts at
  the pairing threshold and follows its comparisons with the rest of the track; it is its
  `confidence` on the track (was always 1). A split needs it at `split_probability`
  (0.001) and `m` of the last `n` comparisons outside a 0.9999 gate (a constant-velocity
  model understates a manoeuvring target's error). A track's confidence, the `confidence`
  built-in of the output schema, is `1 − Π(1 − existence · confidence)` over its source
  tracks (existence: the source's own `provenance.confidence`, 1 when it states none).
- **View.** A view whose position comes from a report without motion (a plot) takes course
  and speed from the newest report in the window that has them, so it can be propagated.
- Settings saved before (`chi2_gate`, `drift_mps`, the split `chi2_gate`) are still read:
  a pairing chi-square gate becomes the equivalent gate probability.

Autoferry scores are unchanged from correlation-2 (same tables, and no split suggestions),
with gnn-2 and mht-2 tracks paired as gnn-1 and mht-1 tracks were. On the GMTI and GPS
replay of 13 Oct 2015 (Garden Island), correlation-2 paired a GMTI track passing a parked
GPS vehicle at 56 m (speeds 6 m/s and 0) after four position comparisons; correlation-3
does not pair them (`a_moving_track_does_not_pair_with_a_still_one_nearby`). The first
correlation-3 build still paired the same vehicle with a GMTI track that had stopped
reporting, by comparing five new GPS fixes with its one last report; hence the rule that
both sides bring new reports.

### gnn-2 (2026-09-25)

Every track carries its probability of being a real target, and the probability decides
(mht-2 is the same change on the MHT, whose leaves each carry their own):

- **Existence** (IPDA with the assigned plot): a new track starts at the prior
  `birth / (birth + clutter)`; between looks it decays with `target_lifetime_secs`; a plot
  multiplies its odds by `Λ + 1 − Pd`, where `Λ = Pd · g / λ` (the plot's Gaussian
  likelihood against the clutter density); a look without one by `1 − Pd`. With a known
  revisit (auto timing) a miss counts once per revisit, else once per scan.
- **Decisions**: confirmed at `confirm_probability` (0.95, and at least `confirm_hits`
  plots); dropped at `drop_probability` (0.02) or after the drop time without a plot.
  Replaces 3 plots in 5 s (`confirm_within_secs` is still read, no longer used).
- **Measurement error**: each plot's own covariance (its ellipse, else its circular
  error, else `measurement_sigma_m`) instead of a circle.
- **Reports** carry the existence as `provenance.confidence`, the position ellipse, and
  the full position and velocity covariance (`uncertainty.covariance`).
- The detection model (`detection_probability`, `clutter_density`, `birth_density`) is a
  stage setting for both algorithms; the MHT-only settings from before still apply when
  set.

| Scenario | Sensor | GNN best track per vessel | Clutter tracks | MHT best track per vessel | Clutter tracks |
|----------|--------|---------------------------|---------------:|---------------------------|---------------:|
| 2 | lidar (cluster 10 m, Pd 0.8) | 186, 99%; 133, 100% | 0 | 185, 99%; 132, 100% | 0 |
| 2 | radar (clutter 4·10⁻⁶) | 63, 100%; 68, 100% | 18 | 63, 100%; 68, 100% | 14 |
| 16 | lidar | 133, 100%; 114, 100% | 0 | 133, 100%; 115, 100% | 0 |
| 16 | radar | 67, 100%; 62, 100% | 5 | 59, 100%; 60, 98% | 4 |

(gnn-1: 1 and 23 clutter tracks in scenario 2, 1 and 4 in 16.)

### mht-2 (2026-09-25)

The gnn-2 existence model on the MHT: every leaf's history carries its existence, a
chosen leaf reports it as `provenance.confidence`, and it confirms and drops the
hypothesis's track (tree scores still decide which hypothesis is best and which trees
are deleted). Scores are in the gnn-2 table: the same as mht-1 but for one clutter track
more in scenario 16 radar (4, mht-1 3) and the same elsewhere.

### correlation-2 (2026-09-25)

Operators steer correlation, and settings change at runtime:

- **Settings** (approach, mode, gates, M of N, splits) are saved from the Correlation page
  and reloaded by the running engine; the `--correlation` flag only sets the approach until
  some are saved.
- **Suggest mode**: kinematic pairings are proposed instead of made; an operator accepts
  (merge) or rejects (the two never pair). Shared identifiers still pair on their own.
- **Do not pair**: an operator's rejection, split or explicit decision is a DO_NOT_PAIR
  edge between source tracks, checked before every pairing, identifier matches included.
- **Splits**: a source track that disagrees with the rest of its system track (chi-square
  18.4, 5 of the last 6 comparisons) is proposed for a split (or split, with automatic
  splits on). Of two, the one that may not stand alone leaves. A split puts it on a track
  of its own, leaves the rest showing only what they report (identity included), and
  records that the two do not pair again.
- Operators can also split, merge and mark "do not pair" by hand; every decision records
  its actor and the correlation version.

Autoferry scores are unchanged from correlation-1 (same tables), and neither scenario
raises a split suggestion: the crossing in scenario 2 does not make correlated tracks
disagree.

### correlation-1 (2026-09-25)

Identifier pairing behind a kinematic sanity gate; kinematic pairing (chi-square gate,
4 of 5 comparisons within 30 s, vetoed by conflicting identifiers or domains with
`kinematics-metadata`); a sensor's concurrent tracks never pair; merges keep the
published track, then the older; best source per field group; sticky identity;
detection association (nearest first, one plot per track per scan); sources end their
tracks with `state: dropped`; only tracks a stand-alone source reports for are published.

| Scenario | Input | Paired right | Wrong | Unpaired | Clutter paired |
|----------|-------|-------------:|------:|---------:|---------------:|
| 2 | fixture tracks | 4 | 0 | 0 (2 same-sensor duplicates kept apart) | 0 |
| 2 | gnn-1 tracks | 5 | 0 | 0 | 0 |
| 2 | mht-1 tracks | 5 | 0 | 0 | 0 |
| 16 | fixture tracks | 4 | 0 | 0 | 0 |
| 16 | gnn-1 tracks | 4 | 0 | 0 | 0 |
| 16 | mht-1 tracks | 4 | 0 | 0 (1 same-sensor duplicate kept apart) | 0 |

| Scenario | Raw detections to the right track | Wrong | Missed | Clutter associated |
|----------|-------------------------------:|------:|-------:|-------------------:|
| 2 | 457 / 531 | 0 | 74 (64 a second plot that scan) | 0 / 318 |
| 16 | 383 / 407 | 0 | 24 (21 a second plot that scan) | 0 / 150 |

### gnn-1 (2026-09-25)

Constant-velocity Kalman filter per track in its local frame; least-cost assignment per
scan (Hungarian method, cost = Mahalanobis distance + ln|S|); confirm after 3 plots in
5 s; drop after 3 s (tentative) or 8 s (confirmed) without one, reported as
`state: dropped`; optional plot clustering; classification at most unknown affiliation
and a domain.

| Scenario | Sensor | Best track per vessel (reports, purity) | Clutter tracks |
|----------|--------|-----------------------------------------|---------------:|
| 2 | lidar (cluster 10 m) | 185, 99%; 133, 100% | 1 |
| 2 | radar | 63, 100%; 68, 100% | 23 |
| 16 | lidar (cluster 10 m) | 133, 100%; 115, 100% | 1 |
| 16 | radar | 68, 100%; 62, 100% | 4 |

### mht-1 (2026-09-25)

Track-oriented MHT on the same filter and lifecycle: log-likelihood-ratio scores,
the best global hypothesis solved exactly per conflict cluster (branch and bound),
N-scan pruning (N = 3), duplicate-tree removal, track numbers that carry over when the
hypothesis moves between trees.

| Scenario | Sensor | Best track per vessel (reports, purity) | Clutter tracks |
|----------|--------|-----------------------------------------|---------------:|
| 2 | lidar (cluster 10 m) | 185, 99%; 132, 100% | 0 |
| 2 | radar | 63, 100%; 68, 100% | 14 |
| 16 | lidar (cluster 10 m) | 133, 100%; 115, 100% | 0 |
| 16 | radar | 59, 100%; 60, 98% | 3 |

On all nine Autoferry scenarios (Python spike, same models): MHT had 15–25% fewer false
tracks than GNN with both sensors fused, similar accuracy, about 10× the compute.
