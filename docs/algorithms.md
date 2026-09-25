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
`OT_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test autoferry -- --nocapture`.

- **Tracker**: per vessel, the reports and purity of its best track; tracks that mostly
  follow clutter.
- **Correlation, sensor tracks**: lidar and radar tracks (from the tracker, or from the
  fixtures' own simple tracker) paired with the vessel track feed: right, wrong,
  unpaired, and clutter tracks paired with a vessel.
- **Correlation, raw detections**: plots associated with the right vessel's track, wrong,
  missed (mostly a second plot on a vessel in the same scan), clutter associated.

## Changelog

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
