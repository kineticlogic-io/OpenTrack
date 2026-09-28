# Changelog

## Unreleased

- **SAPIENT codec** (BSI Flex 335 v2.0): a built-in `sapient` codec plugin (`ot-sapient`,
  decoding with the schema `sapient-rs` ships), framed on TCP by a 4-byte little-endian length
  prefix; example in `docs/examples/sapient.json`.
- SAPIENT codec: enum values inside map fields come out as names; a NaN or infinite float is `null`
  instead of failing the whole message (#30).
- Pipeline designer: the Decode stage shows a protobuf or plugin codec read-only (it is chosen on the
  Transport tab) instead of letting a format replace it (#3).
- Transport form: HTTP poll method and body, the byte order of length framing, the length field's
  adjustment and the gRPC server's keepalive (#6).

## 0.4.0 (alpha), 2026-09-28

### Accreditation: DoD RMF / ASD STIG (800-53 Moderate)

Account, session, audit and web hardening, all configurable in Settings → Security with STIG
defaults.

- **Passwords** (local accounts): 15 characters with upper, lower, digit and special; the last 5 not
  reused; 8 characters changed; 24 h minimum and 60 days maximum age. An admin's password (new
  account, reset, `opentrack user passwd`) is temporary and must change at the next sign-in, as must
  an expired one: the session can do nothing else until then. Existing passwords keep working;
  their age counts from the upgrade.
- **Lockout:** 3 failures in 15 minutes lock an account for 15 minutes (or until an admin unlocks
  it: Settings → Users, `opentrack user unlock`, `POST /api/v1/auth/users/{id}/unlock`).
- **Server-side sessions:** idle timeout 15 minutes (admins 10), measured from the user's own
  activity, not the page's polling; at most 3 per account; list and end your own sessions (admins:
  anyone's) at `/api/v1/auth/sessions`. Sessions from before the upgrade end: sign in again.
- **Inactive accounts** (35 days) are turned off; break-glass accounts can be exempted.
- **Last sign-in notice:** the previous sign-in and the failed attempts since, after signing in.
- **Audit record:** a new append-only, SHA-256 hash-chained `audit` table with every decision
  (mirrored as recorded; undo still marks decisions in place) and every sign-in event, with reason
  and address. Sign-in fails closed when it cannot be recorded. `GET /api/v1/audit` (filters, CSV)
  and `GET /api/v1/audit/verify`; Settings → Audit. Optional retention with a verifiable anchor.
- **Web:** Content-Security-Policy, X-Content-Type-Options, X-Frame-Options, Referrer-Policy,
  Permissions-Policy, HSTS over TLS, `no-store` on the API. Session cookie `SameSite=Strict`;
  `Secure` also behind a TLS proxy (`OT_PUBLIC_TLS=1`).
- **Security labels:** a fused track (and a group) takes the highest classification of its sources
  (correlation setting `labels.classification_order`, default UNCLASSIFIED < CUI < CONFIDENTIAL <
  SECRET < TOP SECRET; U/C/S/TS accepted; an unknown one ranks highest), the union of their
  restrictions and the intersection of their releasability lists (`NONE` when empty). Before, it
  took the highest-priority source's label.
- **NATS TLS:** `OT_NATS_CA` (TLS required), `OT_NATS_CERT` and `OT_NATS_KEY` (mutual TLS).
- **`OT_AUTH=off`** now warns every minute and shows a red banner.
- Nodes sharing their profile must all run this version: older ones refuse correlation settings with
  `labels`.

### FIPS 140-3 cryptography, in every build

See [docs/security/fips.md](docs/security/fips.md).

- **AWS-LC FIPS 3.0** does all of the binary's cryptography: TLS everywhere (rustls' FIPS
  configuration), session and token signatures, password hashes, the NATS `.creds` signature and
  every random secret. The process won't start unless the module is in FIPS mode. Building needs
  Go and CMake.
- **Passwords** are hashed with PBKDF2-HMAC-SHA256 (600,000 iterations). Argon2id hashes from
  earlier versions still verify, and are replaced at the account's next sign-in.
- **SAML** signatures go through OpenSSL with only its validated 3.0.9 FIPS provider, which the
  image builds and forces. SAML sign-in is off wherever OpenSSL is not in FIPS mode.
- **No `ring`:** the rumqttc copy in `third_party/` drops it, and `cargo deny` bans it.

### Encryption in transit

- **Redis over TLS:** `rediss://` URLs, with `OT_REDIS_CA` and, for mutual TLS, `OT_REDIS_CERT`
  and `OT_REDIS_KEY`.

### Supply chain and image

See [docs/security/supply-chain.md](docs/security/supply-chain.md).

- **`cargo deny` in CI** (`deny.toml`): RustSec advisories, a licence allow-list, FIPS bans and
  crates.io only. It led to fixes for:
  - quick-xml in the SAML fork (RUSTSEC-2026-0194 and 0195);
  - webpki in the MQTT client (RUSTSEC-2026-0049, 0098, 0099 and 0104).
- **Other CI checks:**
  - `npm audit`;
  - CycloneDX SBOMs of the binary and the UI (`scripts/security/sbom.py`, uploaded by CI);
  - actions pinned by commit, with a read-only token.
- **The image:**
  - base images pinned by digest, and the Go toolchain checksummed;
  - `HEALTHCHECK`, run by the new `opentrack health`.
- **The compose file** runs the image with a read-only root file system, every capability dropped
  and `no-new-privileges`.
- **Deployment and assessment:**
  - a hardening checklist ([docs/security/hardening.md](docs/security/hardening.md));
  - a control mapping with the open findings
    ([docs/security/stig-mapping.md](docs/security/stig-mapping.md)).

### Findings closed

- **SAML never signs in to a local account:** a sign-on whose email matches an account SAML didn't
  make is refused (before, it signed in as that account, with its role, even admin).
- **Only admins see a source's secrets:** viewers and track managers get passwords, tokens, header
  and metadata values, credentials in URLs and API keys in transport messages as `••••••`;
  `${env:…}` references stay visible.
- **Client certificate revocation:** `OT_TLS_CLIENT_CRL` (files or directories of CRLs; also
  `tls.client_crl_files` on TLS server sources). Revoked, uncovered and stale-listed certificates
  are refused; the lists reload within a minute of a change.
- **Turning an account off revokes its API tokens** for good.
- **Signed releases:** publishing a release builds the image with SLSA provenance and an SBOM,
  pushes it to GHCR, signs it with the project's cosign key (no public log; verify with
  `cosign.pub`), and attaches the SBOMs and digest to the release.
- **Pinned Rust toolchain** (1.98.1).
- `scripts/github/protect-main.sh`: branch protection for `main` (pull requests with one approval,
  CI passing), to apply once the account can.

### Guides and in-app help

- **Guides:** an administrator guide and an operator guide, in `docs/guides/`.
- **Help tab:** the guides are also in the new Help tab, bundled into the build so they work
  offline, and each section can be linked (`#help/<guide>/<section>`).
- **Info tips:** about 160 new ⓘ tips across sources, the pipeline designer, tracks, the registry,
  the schema, correlation, metrics and settings. Tips that were only native tooltips became info
  tips, and stale text was corrected.

## 0.3.4 (alpha), 2026-09-28

### Non-point contacts

Lines of bearing, areas of uncertainty and emitter identities (ELNOT) are fused into tracks. See
[docs/non-point-contacts.md](docs/non-point-contacts.md).

- **The model:** an observation's `geometry` is a `bearing` (from the sensor's position, with its
  error and range) or an `area` polygon. Mapping destinations `geometry.bearing_deg`, `sigma_deg`,
  `max_range_m`, `elevation_deg` and `geometry.polygon`.
- **Association:** a bearing goes to a track when its emitter identity matches, or once its emitter
  has been found on the track: lines from three or more sensors pointing at its precise position,
  clearly likelier than at any other track, or a cross-fix that pairs with it. It adds evidence and identity
  but never moves the track. An area, or an ellipse over 2 km, pairs only with the one track it
  holds.
- **Cross-fixing:** bearings from several sensors are crossed by weighted least squares into fixes
  with an honest error ellipse. The fixes start or update tracks as reports of the built-in `fix`
  source. Ghosts are kept out by emitter identity, three-sensor consensus with outlier tests,
  repetition, and rejecting sets that other tracks already explain.
- **Output:** tracks carry `area` and `bearings`; bearings no track took are published live on
  `contacts.bearing.<source>.<key>`.
- **Map and track card:** a selected track's bearing lines and area; the card lists its bearings.
- **Benchmark** (`esm-crossfix`): 98.9% of 2,297 bearings to the right track, 10 of 10 emitters
  without AIS tracked, 0.44% ghost fixes, 92% of fixes within 2σ, 3 of 20 ships left with a
  duplicate track; ELINT areas never paired with the wrong track (38 paired, 62 alone). Videos render from the run (`scripts/benchmark/replay/esm-video.py`).
- **Single-sensor location:** one moving ESM sensor locates what it hears from its own motion. It
  fits a fixed emitter, or a moving one (bearings-only target motion analysis) when the bearings
  say it moves, with an error that counts what the model can't see. The location enters the
  picture as a `fix` report keyed by ELNOT, so ELINT and video converge on it. Scenario
  `esm-patrol`: a patrol aircraft's ESM locates a fast boat with no AIS at 285 s; its ELINT joins
  at 460 s and its video 15 s after it starts, leaving one track 20 m from the truth, with one
  track number throughout.
- **Emitter motion per source:** `emitter_motion` (`manoeuvre_mps2`, `max_speed_mps`) says how the
  emitters a bearing source hears may move; it bounds single-sensor location's error. Default:
  surface traffic, 0.1 m/s² and 30 m/s. Set in the source's Publish settings.
- **An ELNOT is evidence, not identity:** it never pairs or keys tracks by itself, adds 10:1 when
  two reports share one, and counts for nothing when they differ (a platform can carry several
  radars). Two lines of the same ELNOT fix only by repeating, like anonymous ones.
- **Bearings carry their platform's position:** every bearing must carry its sensor's position at
  its time, in the same message; OpenTrack does not join it from a navigation feed.
- **Correlation:** a track close to another no longer counts itself in the local density it is
  judged against, so a precise track (video) pairs with a wider one (ELINT) beside it.
- **Open water:** set `kinematic.object_density_per_km2` to the real density; the default (1 per km²)
  keeps kilometre-uncertain fixes from pairing by position.

## 0.3.3 (alpha), 2026-09-27

### Protobuf inputs over gRPC

A producer's `.proto` files are uploaded with the source and compiled when it runs, with no `protoc`,
no generated code and no rebuild. Its messages decode into records the mapping reads. See
[docs/protobuf-grpc.md](docs/protobuf-grpc.md).

- **The `protobuf` codec** works over any transport: TCP with varint length framing, UDP, MQTT,
  WebSocket, recorded files, or gRPC.
  - Field names stay as written in the `.proto`, 64-bit integers are numbers, and fields at their
    default are included (a latitude of 0 is 0).
  - A `Timestamp` is an RFC 3339 string.
- **`grpc_client`:** OpenTrack calls a producer's streaming method.
  - The request is written as JSON, and metadata can carry `${env:}` secrets.
  - TLS or mutual TLS, HTTP/2 keepalive pings, and a message size limit.
  - When the producer hangs up or the connection drops, OpenTrack calls again with jittered backoff.
- **`grpc_server`:** producers call methods of their own `.proto`.
  - A bearer token, TLS or mutual TLS (each record carries the producer's certificate subject), and
    limits on connections and message size, gzip bombs included.
  - Every message is checked against the schema. A bad one fails the call with INVALID_ARGUMENT
    saying why, rather than being dropped silently.
  - Backpressure through HTTP/2 flow control: nothing is dropped.
  - Refused calls and failed TLS handshakes show in the source's status.
- **Saving a source checks it:** the `.proto` compiles (errors give `file:line`), and the method fits
  the message.
- **Source editor:** add `.proto` files and see them compile; pick the message, the records field
  and the gRPC method. `POST /protobuf/describe` returns the same for the API.
- **An example:** `docs/examples/grpc/` has a producer on Google's `grpcio` and source specs for both
  directions.
- **Tested against `grpcio`:**
  - both directions, reconnecting after a producer restart, and mutual TLS;
  - 16,000 records a second pushed for a minute with no loss.
- **Fixed:** with NATS unreachable, shutting down waited forever on the multi-node link.

## 0.3.2 (alpha), 2026-09-27

Multi-node: several OpenTrack nodes (server sites, or a drone swarm) share one track
picture with no node in charge. The design is in [docs/multi-node.md](docs/multi-node.md), and the
messages the nodes exchange are specified in [docs/sync-icd.md](docs/sync-icd.md).

### One picture, one number, one reporter

- **Each node reports what its own sensors see**, never what it heard from other nodes, so no node
  gets its own data back disguised as a second sensor.
- **Reporting responsibility:** for each track, the node that sees it best (track quality 0–15,
  from its position error) reports it. The others hold it quietly.
  - A node claims a track only when it beats the reporter's quality by 2.
  - A reporter yields to a better report, and a silent one is taken over after 24 s.
  - A node leaving releases its tracks at once.
- **Dead reckoning sets the rate:** a track is sent when the others' prediction of it drifts past
  50 m (200 m in the air), and otherwise every 12 s.
- **Two numbers for one object become one.** Every node keeps the number with the older origin
  (then the lower UID), whatever it has published. A `delete` on NATS carries `merged_into`, so
  consumers can move their references.

### Track management holds on every node

- **What replicates:** pair, unpair, merge, do-not-pair, delete, groups, deleting a history point,
  and undo.
- **How:** each is logged under a global id (`<site>:<seq>`) and a hybrid logical clock stamp, and
  applied on every node.
  - Conflicting decisions settle the same way everywhere: the later decision on the same tracks
    wins.
  - A decision about a track a node has not heard of yet waits for it (up to 12 h).
  - Undo works across nodes.
- **Receive-only nodes** (a drone with no operator) apply other nodes' decisions and accept none of
  their own.
- **The profile is shared:** publishing an output schema or saving correlation settings applies on
  every node. A local schema draft survives another node's publish.

### Nodes exchange messages without trusting the network

- **OpenTrack does not do the networking.** It publishes compact binary messages on its own NATS
  (`ot.sync.out.*`) for a networking package to carry, and hears other nodes on `ot.sync.in.*`.
  - A track report is 40 bytes.
  - Messages are at most 1 KB.
  - Only site codes an admin trusts are listened to.
- **Missed decisions are repaired:** nodes compare which decisions they hold every 5 s and ask for
  gaps from whoever has them. A decision made on one drone reaches a node that was never in range
  of it.
- **`opentrack link`** runs the boundary. It is part of `all`, and idle until sharing is enabled.
- **`opentrack bridge`** carries the messages between server sites' NATS servers. It can also drop,
  delay, duplicate, cap and partition, to stand in for a poor link.
- **`docs/examples/two-sites`**: two sites and a bridge in docker compose.

### Measured: `bench swarm`

`bench swarm` runs K live nodes with overlapping sensors (AIS-like and radar-tracker-like) over the
bridge, and scores every node's published picture against the truth. The gate run used 5 nodes and
600 targets, with 10% loss, 300 ms jitter, 2% duplicate messages, and half the nodes cut off from
the other half for 5 minutes:

| | Result | Gate |
|---|---|---|
| Targets held under the same number on every node | 98.8% | ≥ 98% |
| Duplicate tracks | 0.76% | < 1% |
| One picture again after the cut heals | 2 s | ≤ 30 s |
| Track reports per node | 6.9 kbit/s per 500 tracks | ≤ 20 |
| Coverage (each node holds what any node sees) | 99.7% | |

On a thin link the same swarm was capped at 16 kbit/s per node, with 10% loss. Each node had a sending
budget of 14 kbit/s (Settings → Nodes). With the budget, 98.4% of what any node sees reaches every
node, and 96.6% of targets have one number everywhere; without it, 83% and 61%. The budget
sends the most urgent reports first: new tracks, state changes, then the largest drifts. A drift
within a track's own position error is not sent at all.

### Settings and UI

- **Settings → Nodes:**
  - share the picture, trusted nodes, receive-only, share the profile;
  - each peer's link health and the decisions held from it;
  - how many tracks this node reports.
- **The track card** shows which node reports the track.
- **The Management log** marks decisions made on other nodes.
- **`GET /sync/status`** returns the same, for headless nodes.
- **The Correlation tab** shows a bubble counting suggestions waiting for a track manager (the
  stareSDK Tabs `badge`).

### Upgrading

Migration `0014_sync` adds the replicated log. Nothing is shared until an admin turns sharing on,
and every node needs its own `OT_SITE_CODE`.

## 0.3.1 (alpha), 2026-09-27

Phase 2, "proven at scale", is met: 16,000 live tracks at 1 Hz on one host, and fewer than 2% wrong
pairings on the harbour benchmark.

### Throughput under live load

- **`bench live`** runs the whole server with a Redis and NATS of its own, fed N tracks at 1 Hz, and
  samples it every 10 s: backlogs, memory, CPU, publications a second, and the delay from report to
  publication.
- **16,000 tracks for 10 minutes:**

  | | Before | After |
  |---|---|---|
  | Engine backlog | 3.4 million, growing | at most 200 |
  | Published | 2,000/s | 3,242/s |
  | Delay (p50 / p99) | 142 / 218 s | 1.0 / 2.0 s |

  It uses about one core and 313 MB.
- **Fixed: the engine dropped reports under load.** A timer abandoned the batch in progress. Timers
  now wait until a batch is finished.
- **The writer publishes concurrently:** one Redis read for every track due, 256 publications in
  flight.
- **Deletes and history-point deletions have their own outbox stream.** It is never trimmed, so a
  writer that falls behind no longer loses them.
- **`OT_OBS_WINDOW_SECS`** sets how long observation streams keep reports (default 600 s). That is
  about 4.8 GB of Redis at 16,000 reports a second.

### correlation-5

- **Splits work against slow feeds.** A radar track that followed another vessel could stay on an
  AIS track that reports every 10 s indefinitely. The split check now has its own window,
  `split.window_secs`, and may reuse the other side's last report.
- **Local density:** a close approach counts for less in a crowded harbour
  (`local_density_radius_m`, 500 m).
- **Automatic splits are the default.**
- **The Autoferry lidar uses MHT**, and so does the `lidar-surface` profile.
- **Harbour wrong pairings: 5.36% → 1.51%.** The costs:
  - more radar and AIS tracks stay apart in the Solent (GOSPA 7,151 → 9,805)
  - more identity changes on two Autoferry recordings

  See [docs/algorithms.md](docs/algorithms.md).

### Benchmark

- **IDF1** for system tracks and each tracker.
- **The harbour gate** appears in every summary, with a stricter figure that judges each pairing at
  each moment.

### UI

- **The History tab** has "Show on map", which draws the track's history as a line, and "Zoom to
  track".
- **stareSDK 0.1.8:** MapView lines and camera fitting.

### Upgrading

- No migrations.
- **Correlation settings saved before 0.3.1 keep their `split.automatic`.** Turn it on in the
  correlation form.

## 0.3.0 (alpha), 2026-09-26

The "secure" phase: sign-in and roles, TLS, undo and history. NATS stays the only output.

### Sign-in and roles

- **Every API call needs a signed-in caller**, in one of three roles:
  - `viewer`: sees everything but accounts and secrets
  - `track_manager`: also manages tracks
  - `admin`: also configures OpenTrack
- **One middleware checks the role each path needs.** A new route that changes something is an
  admin's until it is listed. The decision log now records the signed-in account, which a client
  can no longer set.
- **How callers sign in**, modelled on OpenStare's:
  - local accounts (Argon2id passwords) with a signed `ot_session` cookie
  - SAML 2.0 single sign-on, ported from OpenStare
  - OpenStare's own sign-in and API tokens, when OpenTrack runs beside OpenStare
  - API tokens for machines
  - client certificates over TLS
- **UI:**
  - the sign-in page, like OpenStare's
  - the user menu (change password, sign out)
  - Settings → Users (accounts and API tokens)
  - Settings → Security (sessions, SAML, OpenStare sign-in, client certificates)
  - controls a role cannot use are disabled
- **Warning banner:** as OpenStare's, text users accept after signing in. Declining signs them out.
- **`opentrack user`** manages accounts on a node without the UI.
- **The first admin** comes from `OT_ADMIN_EMAIL` and `OT_ADMIN_PASSWORD`. Without them, it is
  `admin@opentrack.local`, with the password in `initial-admin.txt` beside the database.
- **`OT_AUTH=off`** turns sign-in off for development.

### TLS

- **The API and UI over TLS** with `OT_TLS_CERT` and `OT_TLS_KEY`. With `OT_TLS_CLIENT_CA`,
  client certificates sign machines in.
- **Feeds over TLS and mutual TLS:**
  - a `tls` object on the `tcp_client`, `http_poll`, `websocket` and `mqtt` transports: CA, client
    certificate and key, server name
  - a `tls` object on `tcp_server`: certificate, key, and a client CA that makes client
    certificates required

### Undo

- **What undo covers:** a track manager's pair, unpair, delete, merge, split, "do not pair" and
  group changes, from the decision log (`POST /decisions/{id}/undo`).
- **An undo is a decision of its own.**
- **How it works:** it reverses the decision's links in the temporal track graph.
  - A deleted or merged-away track comes back under its UID.
  - A track the engine made meanwhile for the same source retires.
- **When it is refused:** while a later change by someone else to the same tracks stands.

### Position history

- **What is kept:** each system track's published positions, for `history_hours` (default 12 h).
  At most one point is kept every `history_interval_secs` (default 10 s), both in Settings.
  - A point costs about 130 bytes: 2,000 tracks for 12 h at 10 s is about 1.1 GB of Redis.
- **Reading it:** `GET /tracks/{uid}/history`.
- **Delete history point** (`POST /history/{uid}/delete`): a track manager deletes a bad point.
  - It goes out on NATS as `delete_history_point`, on `tracks.history.tms-<UID>.<ms>`.
  - When the point was the track's latest, the track steps back to the point before.
  - **Consumers of `tracks.>` must skip message kinds they do not handle**
    ([docs/nats-output.md](docs/nats-output.md)).

### Engine

- **One SQLite connection** (0.2.2) and batched writes keep the engine at about 25,000 to 29,000
  observations a second from 2,000 to 16,000 tracks, with history recorded.

### Upgrading

- **Migration 0013** adds accounts, tokens and sign-in settings. It runs on start.
- **Sign-in is on after the upgrade.** Set `OT_ADMIN_EMAIL` and `OT_ADMIN_PASSWORD`, or read
  `initial-admin.txt` beside the database, then sign in.
  - Scripts calling the API need an API token.
- **The image** now needs libxmlsec1, which it installs. `--no-default-features` builds without
  SAML.

## 0.2.2 (alpha), 2026-09-26

### Engine throughput

The engine now handles about 28,000 observations a second, from 2,000 to 16,000 tracks updating
once a second. It handled about 3,900 before.

| Tracks | Before | Engine | Whole run |
|---|---|---|---|
| 2,000 | 3,880/s | 29,400/s | 19,000/s |
| 8,000 | 3,920/s | 27,700/s | 18,100/s |
| 16,000 | not measured | 26,800/s | 17,700/s |

"Whole run" also counts the benchmark decoding and feeding its own data, which the sources
process does in a deployment.

- **One database connection.** The engine keeps one SQLite connection open. It used to open one
  for every decision, and closing it rewrote and deleted the database journal: about 6.5 ms per
  new track, a cap of some 150 new tracks a second.
- **Batched Redis writes.** Within a batch of observations, the engine writes source reports and
  system tracks together in one atomic pipeline, each track once. It used to make about three
  round trips per observation. Publish decisions are still made as each observation arrives.
- **Load scenarios.** `load-2k`, `load-8k` and `load-16k` in `scripts/benchmark/` feed tracks at
  1 Hz for a minute and report observations a second.

### Registry spreadsheet

- **A publish column** (always, never, or empty for automatic) comes after status, so a track
  manager's publish override survives an export and an import.
- **A sheet without the column** leaves every override as it is.

### Documentation

- **Headless running.** The README now describes running OpenTrack headless.

## 0.2.1 (alpha), 2026-09-26

### Tracker profiles

- **What a profile is.** A sensor's tracker settings as a JSON file, with:
  - its sensor kind and platform
  - where the numbers come from
  - the tracker stage it sets
- **Where they live.**
  - `profiles/trackers/` ships with OpenTrack (`OT_PROFILES_DIR`).
  - Imported profiles go to `profiles/trackers` beside the database.
- **In a source's tracker stage** you can load a profile, save the tuned stage as one, import a
  file, or download one. The stage records which profile it came from and shows when it has
  changed since.
- **API.** `GET/POST /tracker-profiles`, `GET/DELETE /tracker-profiles/{name}`. Imports and deletes
  go in the decision log.
- **Benchmark.** `bench run --profile SOURCE=NAME`.
- **The shipped profiles** span the sensor kinds with a tracker stage:
  - GMTI:
    - `gmti-wide-area`: clutter density 2e-7, a balance between noise and real traffic
    - `gmti-high-clutter`: 1e-6, for very noisy data such as the Garden Island recordings
    - `globalhawk-mti`, `lynx-gmti`, `astor-dmti`: nominal starting points
  - Maritime MTI: `lsrs-maritime-mti`
  - Radars: `marine-radar-x`, `coastal-surveillance-radar`, `air-surveillance-radar`
  - Lidar: `lidar-surface`
- **The STANAG 4607 example** uses `gmti-wide-area`. Its old clutter density, 3e-8, made about 1,150
  tracks in 40 minutes on the Garden Island recordings.

### correlation-4

Radar tracks now join their AIS and ADS-B tracks. On the benchmark:

| Scenario | Recall | Precision | GOSPA |
|---|---|---|---|
| AIS + coastal radar | 49% → 78% | 98% → 96% | 11,869 → 7,151 |
| ADS-B + radar | 50% → 90% | 100% | 17,866 → 6,862 |

The changes:

- A slow feed's last report is reused against a sensor's newer ones, weighted and at most every
  10 s.
- Stopped targets are propagated as a slow drift.
- Aircraft get their own velocity spread.
- Views up to 180 s old are compared.
- The same-source window has its own setting.

The new settings are in the correlation form. See [docs/algorithms.md](docs/algorithms.md).

## 0.2.0 (alpha), 2026-09-26

### Plugins

You can now add codecs, trackers and pairing scorers of your own. The full guide is
[docs/plugins.md](docs/plugins.md).

- **Interface.** The plugin interface is `wit/plugin.wit` (`opentrack:plugin@0.1.0`) and covers
  three kinds of plugin:
  - **codec**: turns frames into records.
  - **tracker**: turns plots into tracks, as a source's tracker stage (`"algorithm": "plugin"`).
  - **scorer**: scores whether a report and a system track are the same object. The engine keeps
    its own pairing test, thresholds and decisions. Each merge decision records the plugin, its
    version and what it said.
- **Two ways to run a plugin**, and the engine treats them the same:
  - **WebAssembly components** run in OpenTrack's process, sandboxed. You set each plugin's grants:
    memory, time per call, directories, network addresses and environment. Every stream gets its
    own instance of the plugin.
  - **External programs** serve the same interface over a socket. Use these for Python with numpy
    or Stone Soup, or for a GPU.
- **Settings → Plugins** lets you:
  - add a component or an external plugin's address
  - enable, disable, grant, check and delete plugins
  - see where each plugin is used

  Every role picks up changes within 5 s, and every change goes in the decision log. The tracker
  stage and the correlation settings choose plugins with forms built from each plugin's manifest.
- **Command line.** `opentrack plugin check|add|list`.
- **SDKs** (`sdk/`):
  - **Rust** builds components. Examples: an alpha-beta tracker, a CSV codec and a speed-veto
    scorer.
  - **Python** serves plugins externally, or builds plain Python into components with
    componentize-py. Examples: a domain and course scorer, and a Stone Soup GNN tracker.
- **Built-in plugins.** STANAG 4607 is now a built-in plugin in the same registry.

### Benchmark

- **`opentrack bench`** runs a recorded scenario through the real pipelines and engine, on the
  scenario's own clock and as fast as they go, and publishes nothing.
- **`scripts/benchmark/`** builds and scores 27 scenarios, compares runs, and sweeps settings
  (`--set`) and plugins (`--plugin`). The scenarios are:
  - nine Autoferry lidar and radar recordings, each with and without a vessel feed
  - Garden Island GMTI with the exercise GPS
  - Stone Soup's Solent AIS and OpenSky ADS-B with simulated radars
  - a synthetic case of targets crossing in heavy clutter
- **Scores** cover:
  - GOSPA
  - SIAP-style completeness, false tracks, accuracy and identity
  - track counts
  - correlation precision and recall
- **Data stays out of the repository**, in `$OT_BENCH_DATA`.
- The replay tools moved to `scripts/benchmark/replay/`.

### Engine

- **Scenario clock.** Lifecycle and timers follow the scenario's clock in a replay.
- **Non-blocking reads.** A zero read block now returns at once. Redis checks BLOCK timeouts only
  about every 100 ms, which made fast replays 30 times slower.

### Upgrading

- **Migration 0012** adds the `plugins` table. It replaces an unused placeholder, and runs on
  start.
- **Container builds** now copy `wit/`.

## 0.1.0 (alpha), 2026-09-26

The first alpha has:

- **Sources**:
  - transports: TCP, UDP, HTTP polling, WebSocket, MQTT and file
  - codecs: JSON, XML, Cursor-on-Target and STANAG 4607
  - a pipeline designer with live preview
- **Trackers**: GNN and MHT, each track with an existence probability.
- **Correlation** (correlation-3): covariance-propagated likelihood-ratio pairing, splits, and a
  decision log and track graph.
- **Entities**: one Entity model, with pipeline links in either direction and a registry
  spreadsheet.
- **Track management**: pair, merge, group (battle groups, flights, convoys with 2525C echelon
  symbols), delete and designate.
- **Output**: NATS JetStream in an OTH-GOLD style message.
- **UI**: React on stareSDK.
