# OpenTrack administrator guide

For the people who install, configure, secure and keep OpenTrack running. The people who work on
the picture day to day have the [operator guide](operator.md). Written for OpenTrack **0.3.4**.

Every section has a short, stable anchor, so the ⓘ tips in the UI and other documents can link to
it (in the app: `#help/admin/<anchor>`). An anchor is the heading's slug, by the rule GitHub uses:
lowercase, every character that is not a letter, digit, space, hyphen or underscore removed, and
spaces turned into hyphens. Where a heading cannot give the anchor wanted, an `<a id="…"></a>` line
just above it sets the anchor instead. Anchors are unique within a guide. Don't rename a heading
without keeping its anchor.

## Overview

OpenTrack is one binary, `opentrack`. Each subcommand runs one **role**:

| Role | What it does | Needs |
|---|---|---|
| `serve` | the REST API (`/api/v1`) and the web UI, on `OT_BIND` (default `0.0.0.0:8090`) | SQLite, Redis, NATS |
| `sources` | every enabled source: transports, decoding, mapping, trackers | SQLite, Redis |
| `engine` | correlation, track lifecycle, track management, groups | SQLite, Redis |
| `writer` | publishes system tracks from the Redis outbox to NATS JetStream | Redis, NATS |
| `link` | exchanges tracks and decisions with other OpenTrack nodes (idle until enabled) | SQLite, Redis, NATS |

`opentrack all` runs all five in one process. That is how most sites run it.

Two stores hold the state:
- **SQLite** holds everything a person decided or configured: sources and their revisions, the
  output schema, the entity registry, correlation and instance settings, accounts, plugins, the
  decision log and the track graph.
- **Redis** holds everything the feeds produce: observation streams, live source and system tracks,
  the outbox, position history and metrics. See [Redis](#redis).

The API sends track management to the engine as commands through Redis and waits up to 10 s for
the answer. So the roles can run in one process or in several.

## Installation

### Requirements

- **Redis** (any recent version). OpenTrack keeps its keys under one prefix (`OT_REDIS_NAMESPACE`,
  default `tms`), so it can share a Redis with other applications.
- **NATS with JetStream**, to publish tracks. Without it, OpenTrack runs, but nothing reaches
  consumers.
- **To build from source:** Rust (stable, see `rust-toolchain.toml`) and Node 22. SAML needs
  `libxmlsec1` (the Docker image has it). A lean build without SAML needs neither: `cargo build
  --release --no-default-features`.

### Docker compose

The `docker-compose.yml` in the repository runs `opentrack all` with host networking, beside an
existing Redis, publishing to an existing NATS (OpenStare's, as a rule).

1. Create a `.env` file next to `docker-compose.yml`. Compose refuses to start without it. Put
   your `OT_*` settings and secrets in it (see [Configuration](#configuration)). At least set
   `OT_SITE_CODE`, and `OT_ADMIN_EMAIL` with `OT_ADMIN_PASSWORD`.
2. Make the data directory writable by uid 1000, the user the container runs as. By default it is
   `./data`; set `OT_DATA_DIR` to put it elsewhere. It is mounted at `/data`.
3. Start it: `docker compose up -d --build`.
4. Open `http://<host>:8090` and sign in.

For a local NATS with JetStream while testing: `docker compose --profile dev-nats up -d nats`.

In the image, `OT_SQLITE_PATH` is `/data/opentrack.db`, `OT_UI_DIR` is `/opt/opentrack/ui` and
`OT_PROFILES_DIR` is `/opt/opentrack/profiles/trackers`. Run the command-line tools inside the
container, for example `docker compose exec opentrack opentrack user list`.

### From source

```sh
cargo build --release
(cd ui && npm ci && npm run build)
./target/release/opentrack all
```

Run it from the repository root, or set `OT_UI_DIR` and `OT_PROFILES_DIR`: their defaults
(`ui/dist`, `profiles/trackers`) are relative to the working directory. Without a built UI,
`serve` and `all` serve the API alone.

To check the whole pipeline end to end, push a synthetic track through it, verify it arrives in
the NATS stream, then retire it: `opentrack synthetic --verify --retire`.

### First start

On its first start OpenTrack:
- creates and migrates the SQLite database at `OT_SQLITE_PATH` (and its directory);
- makes the session signing key, `session.key`, beside the database, unless `OT_SESSION_SECRET` is
  set;
- makes the first admin account (see [First admin](#first-admin));
- creates the JetStream stream `OT_NATS_STREAM` if it is missing. It never changes an existing
  stream.

Then sign in, and in **Settings**:
1. set the site name and, if needed, the classification banner ([Banners](#banners));
2. add accounts ([Users](#users)) and set up single sign-on ([Sign-in](#sign-in));
3. add sources (the **Sources** tab), and publish an output schema (the **Schema** tab).

### Headless

OpenTrack needs no UI and no operator. Everything the UI does is a REST call, and configuration can
also come from files:
- source specs as JSON (`docs/examples/` has several), posted to `/api/v1/sources`;
- tracker profiles as JSON files in `profiles/trackers/`;
- plugins with `opentrack plugin add`;
- accounts with `opentrack user` ([User commands](#user-commands)).
- or everything at once, from another node's export: `opentrack config import <file>`
  ([Rebuild a node](#rebuild-a-node-from-a-configuration-export)).

A node that only collects and correlates needs `sources` and `engine`, plus `writer` to publish.
It can start from a database prepared elsewhere (`OT_SQLITE_PATH`). Give every node its own
`OT_SITE_CODE`, so track numbers never clash.

## Command line

`opentrack [common options] <command> [options]`. The common options (the database, Redis, NATS,
site code and the rest of [Core settings](#core-settings)) come before the command, or from their
`OT_*` variables. `opentrack --help` and `opentrack <command> --help` list every option.

### Server roles

| Command | What it runs |
|---|---|
| `migrate` | Creates or upgrades the SQLite database, prints its schema version, and exits. |
| `serve` | The control plane: REST API and UI. |
| `sources` | Every enabled source. |
| `engine` | The engine. Run exactly one per Redis namespace: it owns the live picture. |
| `writer` | The writer. Several may run; give each its own `OT_WRITER_CONSUMER`. |
| `link` | This node's side of the link to other nodes ([Multi-node](#multi-node)). |
| `bridge` | Carries sync messages between nodes' NATS servers, for server sites with no networking package of their own: `opentrack bridge --node nats://a:4222 --node nats://b:4222`. It can also drop, delay, duplicate, cap and partition messages, for tests (`--loss`, `--delay-ms`, `--jitter-ms`, `--duplicate`, `--rate-kbps`, `--partition`). |
| `all` | `serve`, `sources`, `engine`, `writer` and `link` in one process. It migrates the database once before they start. |

### Other commands

| Command | What it does |
|---|---|
| `synthetic` | Pushes synthetic tracks through the pipeline: `--count`, `--lat`, `--lon`, `--move-secs`; `--verify` waits until each is in the NATS stream, `--retire` retires them afterwards. To test without touching operational subjects, give it and the writer their own `OT_NATS_STREAM`, `OT_NATS_TRACKS_SUBJECT` and `OT_REDIS_NAMESPACE`. |
| `bench` | Runs a recorded scenario through the real pipelines and engine as fast as they go and writes the results (`opentrack bench <scenario dir> --out <dir>`). Publishes nothing. See `scripts/benchmark/README.md`. |
| `config export [--out <file>]` / `config import <file>` / `config status` | Writes the whole configuration (secrets included) to standard output or a file; rebuilds an empty node from one; says whether this node is empty. See [Configuration export](#configuration-export). |
| `retire <uid>` | Retires one system track: records the decision (`--reason`), closes its links in the track graph and queues its delete for the writer. Takes `OTK000000042` or `tms-OTK000000042`. |

### Plugin commands

- `opentrack plugin check <file.wasm | address> [--grants '<json>']` loads a plugin, opens each
  kind it provides with its default options, and prints its manifest and what worked. It changes
  nothing and exits non-zero when something failed.
- `opentrack plugin add <file.wasm | address> [--grants '<json>'] [--replace]` adds it, as
  Settings → General → Plugins does. `--replace` installs a new build of a plugin that is already added.
- `opentrack plugin list` lists the plugins added and whether each is enabled.

An address is `host:port`, `tcp://host:port` or `unix:/path`. Grants are JSON, for example
`'{"memory_mb": 512}'`. See [Plugins](#plugins).

### User commands

For a node without the UI, or when no admin can sign in. Each change goes in the decision log as
the actor `cli`. The commands open the database directly, so they work while the server runs.

| Command | What it does |
|---|---|
| `opentrack user list` | Every account: email, role, origin (`local` or `saml`), active or off, and whether it has a password. |
| `opentrack user add <email> --role <role> [--name <name>] [--password-stdin]` | Adds an account. With `--password-stdin` the password is the first line of standard input (at least 8 characters). Without one, the account can only use single sign-on or an API token. |
| `opentrack user role <email> <role>` | Changes the role: `viewer`, `track_manager` or `admin`. |
| `opentrack user passwd <email>` | Sets the password from standard input. The account's sessions and API tokens end. |
| `opentrack user disable <email>` / `enable <email>` | Turns the account off (its sessions and tokens stop working at once) or on. |
| `opentrack user token <email> --name <what for> [--days 365]` | Prints a new API token that acts as the account. It must be signed with the server's key: run it with the same `OT_SESSION_SECRET`, or next to the same `session.key`. |

For example, from a file only you can read:

```sh
opentrack user passwd admin@example.org < /secure/new-password.txt
```

## Configuration

Every setting is a command-line option or an `OT_*` environment variable. With Docker, put them in
`.env`. Settings people change while OpenTrack runs (sources, schema, correlation, banners,
accounts, sign-in) are not here: they live in the database and are changed in the UI or the API.

An empty variable counts as unset for the sign-in and TLS settings below, because Compose passes
unset variables as empty ones.

### Core settings

For every role.

| Variable | Default | |
|---|---|---|
| `OT_SQLITE_PATH` | `data/opentrack.db` | The SQLite database. Created and migrated on start, with its directory. Other files live beside it ([Data directory](#data-directory)). |
| `OT_REDIS_URL` | `redis://127.0.0.1:6379` | |
| `OT_REDIS_NAMESPACE` | `tms` | Prefix of every Redis key OpenTrack owns. |
| `OT_REDIS_CA` | | TLS to Redis (with a `rediss://` URL): trust this CA (PEM) instead of the system's roots. |
| `OT_REDIS_CERT`, `OT_REDIS_KEY` | | Mutual TLS to Redis: this client certificate and key (PEM). |
| `OT_SITE_CODE` | `OTK` | The GOLD site code that starts every track number (3 characters, A–Z and 0–9). Fixed for a deployment: changing it changes every new track number. Every node needs its own. |
| `OT_PROFILES_DIR` | `profiles/trackers` | Tracker profiles shipped with OpenTrack (read only). Profiles imported in the UI go to `profiles/trackers` beside the database instead. |
| `OT_OBS_WINDOW_SECS` | `600` | How long each source's observation stream keeps reports, in seconds (at least 10). The engine can be down this long without losing any. Redis holds about 500 bytes a report: 16,000 reports a second for 600 s is about 4.8 GB. |

### NATS output

For `serve`, `writer`, `link` and `synthetic`.

| Variable | Default | |
|---|---|---|
| `OT_NATS_URL` | `nats://127.0.0.1:4222` | NATS server(s), comma separated. |
| `OT_NATS_CREDS` | | A credentials file (JWT and NKey). |
| `OT_NATS_TOKEN` | | A token. |
| `OT_NATS_USER`, `OT_NATS_PASSWORD` | | A user and password. |
| `OT_NATS_CA` | | TLS to NATS: trust this CA (PEM), and refuse a connection without TLS. |
| `OT_NATS_CERT`, `OT_NATS_KEY` | | Mutual TLS to NATS: this client certificate and key (PEM). |
| `OT_NATS_STREAM` | `TRACKS` | The JetStream stream. Created if missing, never modified. |
| `OT_NATS_TRACKS_SUBJECT` | `tracks` | Subject prefix: each track is published on `<prefix>.tms-<UID>`. |
| `OT_NATS_MAX_AGE_HOURS` | `24` | Message age limit of a stream OpenTrack creates. Live tracks are republished long before this. |

The message consumers receive is described in [docs/nats-output.md](../nats-output.md).

### Control plane

For `serve` (and `all`).

| Variable | Default | |
|---|---|---|
| `OT_BIND` | `0.0.0.0:8090` | Address the API and UI listen on. |
| `OT_UI_DIR` | `ui/dist` | The built UI, served at `/`. Skipped when it has no `index.html`. |
| `OT_AUTH` | `on` | `off` turns sign-in off: every caller is an admin. Development only ([Sign-in turned off](#sign-in-turned-off)). |
| `OT_ADMIN_EMAIL`, `OT_ADMIN_PASSWORD` | | The first admin account, made only while there are no accounts ([First admin](#first-admin)). |
| `OT_SESSION_SECRET` | `session.key` beside the database | The key sessions and API tokens are signed with, at least 32 characters. Unset, one is made on first start and kept in `session.key`. Changing it signs everyone out and voids every API token. |
| `OT_PUBLIC_URL` | | Where browsers reach OpenTrack, such as `https://opentrack.example.org`. SAML needs it. An `https://` address also marks cookies `Secure` and sends HSTS. |
| `OT_PUBLIC_TLS` | off | `1`: a proxy in front of OpenTrack ends TLS, so cookies are `Secure` and HSTS is sent. |

### TLS

For `serve`. With a certificate, the control plane serves HTTPS only, and session cookies are
marked `Secure`.

| Variable | |
|---|---|
| `OT_TLS_CERT` | The server certificate (PEM, with its chain). Needs `OT_TLS_KEY`. |
| `OT_TLS_KEY` | Its private key (PEM). |
| `OT_TLS_CLIENT_CA` | Accept client certificates this CA (PEM) signed, as the accounts [Client certificates](#client-certificates) maps them to. A client certificate is optional: browsers without one still sign in with a password or single sign-on. |
| `OT_TLS_CLIENT_CRL` | Certificate revocation lists for client certificates: PEM or DER files, or directories of them, comma separated. A revoked certificate, one no list covers, or one whose issuer's list is past its next update is refused. Reloaded within a minute of a change. |

TLS for feeds is set per source, in the source's transport (see the README, "Sources and
pipelines").

### Engine and writer

| Variable | Default | |
|---|---|---|
| `OT_ENGINE_CONSUMER` | `engine-1` | The engine's consumer name on the observation streams. |
| `OT_CONFIRM_AFTER` | `3` | Reports a new system track needs before it is confirmed (at least 1). A source's own **Confirm after** (in its publish stage) overrides it for that source's tracks. |
| `OT_DROP_AFTER_HOURS` | `6` | Hours without a report before a system track is dropped and deleted downstream. |
| `OT_CORRELATION` | `kinematics-metadata` | How tracks with no shared identifier pair, until someone saves correlation settings: `identifiers` (never), `kinematics`, or `kinematics-metadata` (kinematics, vetoed by conflicting identifiers or domains). Once correlation settings are saved in the Correlation tab, the saved ones win and this is ignored; the engine logs a warning at start when it is set and the saved approach differs. |
| `OT_WRITER_CONSUMER` | `writer-1` | The writer's consumer name; unique per writer. |
| `OT_WRITE_MIN_INTERVAL_SECS` | `5` | At most one publication of a track this often. Significant changes (identity, classification, state) go at once. |

A track is marked lost when its sources stop reporting: after 60 s for air and space tracks,
30 minutes for subsurface, 15 minutes for the rest. It is dropped after `OT_DROP_AFTER_HOURS`.

### Multi-node sync

For `link`. See [Multi-node](#multi-node).

| Variable | Default | |
|---|---|---|
| `OT_SYNC_PREFIX` | `ot.sync` | Subject prefix of the sync boundary on this node's NATS: `<prefix>.out.<kind>` and `<prefix>.in.<kind>`. |
| `OT_SYNC_SUMMARY_SECS` | `5` | Seconds between the summaries that let nodes find the decisions they missed. |

### Logging

| Variable | Default | |
|---|---|---|
| `OT_LOG` | `info` | Log filter, in `tracing` syntax: `warn`, `debug`, or per module, such as `info,opentrack=debug,ot_source=debug`. |
| `OT_LOG_FORMAT` | text | `json` writes one JSON object a line, for log collectors. |

### Test-only variables

These are read only by the test suites and benchmark tools, never by a running server:
`OT_TEST_REDIS_URL`, `OT_TEST_NATS_URL`, `OT_TEST_MQTT_URL`, `OT_GMTI_SAMPLES`, `OT_BENCH_DATA`,
`OT_BENCH_GMTI`, `OT_BENCH_GPS`, `OT_REPLAY_TRACE` and `OT_ESM_NAIVE`. `OT_DATA_DIR` is read
only by `docker-compose.yml` (the host directory mounted at `/data`).

## Data directory

The directory that holds the database (`data/` from source, `/data` in Docker):

| File | What it is |
|---|---|
| `opentrack.db` | The SQLite database: sources and their revisions, the output schema, the registry and its revisions, correlation and instance settings, sign-in settings, accounts, API token records, plugins (WebAssembly files included), groups, correlation suggestions, the decision log, the track graph and the UID sequence. |
| `opentrack.db-wal`, `opentrack.db-shm` | SQLite's write-ahead log and its index, while the database is open. They are part of the database: never delete them while OpenTrack runs, and never copy the `.db` file alone while it runs ([SQLite backup](#sqlite-backup)). |
| `session.key` | The key sessions and API tokens are signed with, when `OT_SESSION_SECRET` is not set. Readable by its owner only. Lose it and everyone is signed out, and every API token stops working. |
| `initial-admin.txt` | The first admin's made-up password, when no `OT_ADMIN_*` was given ([First admin](#first-admin)). Delete it once you've changed that password. |
| `profiles/trackers/` | Tracker profiles imported or saved in the UI. |

### Redis

Everything under `<namespace>:` (`tms:` by default):

| Keys | What they hold |
|---|---|
| `tms:obs:<source>` | Each source's observation stream, kept for `OT_OBS_WINDOW_SECS`. |
| `tms:src:…`, `tms:sys:<uid>` | Live source tracks and system tracks: the picture itself. |
| `tms:thist:<uid>` | Each system track's position history ([Instance settings](#settings-tab)). |
| `tms:out`, `tms:out:ctl` | The writer's outbox: updates, and the deletes and history-point deletions (never trimmed). |
| `tms:cmd`, `tms:cmdres:<id>` | Commands from the API to the engine, and the answers. |
| `tms:cache:…` | Cached static data (AIS static reports) for the static join. |
| `tms:status:<source>`, `tms:metrics:…` | Source status, and per-minute metrics (kept for about a week). |
| `tms:sync:…`, `tms:contacts:out` | Messages for other nodes, and bearings no track took, for the writer. |

Redis holds nothing a person decided; it is all rebuilt from the feeds. But the live picture and
its track numbers are there. Lose Redis, and tracks form again under new numbers (never ones
already issued), and the engine publishes deletes for this node's tracks left in the NATS stream.
See [Restore](#restore).

## Users

### Roles

Each role includes the ones before it. The server checks the role on every call, from the account
as it is now, so a role change or a deactivation takes effect at once.

| Role | What it may do |
|---|---|
| `viewer` | See everything: tracks, sources, the registry, the schema, correlation, settings and metrics. Export tracks and the registry. Change nothing but their own password. Viewers cannot see accounts, API tokens, sign-in settings or the configuration export. |
| `track_manager` | Also manage the picture: pair, unpair, merge, split, do-not-pair, delete, group, designate tracks (edit and pin entities), accept and reject correlation suggestions, undo those decisions, delete history points, and edit and import the registry. |
| `admin` | Also configure OpenTrack: sources, the output schema, correlation settings, instance settings and banners, plugins, tracker profiles, nodes, accounts, API tokens, sign-in, the configuration export and purge. |

In the API: reading needs `viewer`; changes under `/tracks/`, `/groups`, `/registry`,
`/correlation/suggestions/`, `/decisions/` and `/history/` need `track_manager`; every other
change needs `admin`, as do reads under `/auth/` (other than your own profile), `/export/config`,
`/import/config`, `/probe` and `/audit`.

Every change records the account that made it in the decision log.

### First admin

When the database has no accounts, `serve` makes an admin:
- from `OT_ADMIN_EMAIL` and `OT_ADMIN_PASSWORD` (at least 8 characters, or the server refuses to
  start); or, without them,
- `admin@opentrack.local`, with a made-up password written to `initial-admin.txt` beside the
  database. The log says where.

Sign in with it, change the password (the account menu, top right), and delete
`initial-admin.txt`. Once any account exists, `OT_ADMIN_*` is ignored: it never resets a
password.

### Adding accounts

In **Settings → Users**, **Add account**: email, name, role, and a password that meets the
[password policy](#password-policy). Leave the password empty for an account that signs in only with single sign-on.
Accounts that SAML makes appear here on their first sign-on, with the origin `saml`. From the
command line: `opentrack user add`.

### Disabling accounts

In the Users table:
- **Role** changes at once.
- **Active** off: the account can't sign in, and its sessions and API tokens stop working at once.
  Turn it on again to restore them.
- **Delete** removes the account and its API tokens. The decision log keeps what it did.

OpenTrack refuses any change that would leave no active admin, and won't delete your own account.
The table doesn't let you turn your own account off.

A SAML account's role comes from the identity provider: it is set again at every sign-on, so change
it in the provider's mapping, not here.

### Passwords

- **Your own:** the account menu (top right) → **Change password**. Your other sessions end. It
  is offered only to accounts that signed in with a password.
- **Someone else's:** the key button on their row. **Set password** sets a temporary one, which the
  user must change at their next sign-in. **Remove password** makes the account single sign-on
  only. Either way the account's sessions **and API tokens** end. Remember this before resetting
  a service account's password.
- **From the command line:** `opentrack user passwd <email>`, reading the new password from
  standard input. It is temporary too.

Every password must meet the [password policy](#password-policy).

Passwords are hashed with PBKDF2-HMAC-SHA256 in the FIPS module (see
[Security hardening](#security-hardening)). Hashes made before 0.4.0 (Argon2id) still work, and
are replaced at the account's next sign-in. Sign-in attempts are limited per client address: 5 at
once, then one a second.

### API tokens

For scripts and services. A token acts as an account, with that account's role, until it expires
or is revoked.

1. **Settings → API tokens → New token**. Name it after what uses it, pick the account it acts as,
   and how many days it lasts (1 hour to 10 years; default 365).
2. Copy it. It is shown once.
3. Send it as `Authorization: Bearer <token>`.

Revoke it with the bin on its row. A token also stops working when its account is turned off,
deleted, signed out everywhere, or has its password set, and when the session key changes.

A dedicated account per service, with the least role it needs (`viewer` for a monitor), keeps
tokens independent of people's passwords.

### Revoking sessions

To sign an account out everywhere, use **Sign out everywhere** (the door button on its row). Every
session and API token it holds ends now; it can sign in again. From the command line, `opentrack
user passwd` also ends them (turning an account off and on again does not: its tokens work again).

### Users panel

**Settings → Users** (admins only) has two tables:
- **Users**: email, name, role (change it in place; a `saml` account's comes from the identity
  provider, see [SAML](#saml)), origin (`local` or `saml`; **SSO** means no password), active,
  **Never turn off** (a break-glass account, which [inactivity](#inactive-accounts) never turns
  off), state, last sign-in, and the password, sign-out and delete buttons.
- **API tokens**: name, the account it acts as, who made it, when, when it expires, and its state
  (active, expired, revoked).

## Sign-in

OpenTrack accepts several ways of signing in at once. For each request it tries, in order: a client
certificate, an API token, its own session cookie, then (when trusted) OpenStare's session or
token. The **Single sign-on** panel in Settings → Security holds the sign-in settings, in one form
with one **Save**: the **Password sign-in** switch, then a section each for SAML, OpenStare sign-in
and client certificates. Changes take effect at once and go in the decision log.

### Password sign-in

Local accounts sign in with email and password at `/login`. The session is a signed `ot_session`
cookie (HttpOnly, SameSite=Strict, and Secure over TLS). It lasts at most 24 hours; see
[Session limits](#session-limits).

**Password sign-in**, at the top of the Single sign-on panel: off means accounts sign in only with
SAML or OpenStare. You can turn it off only when one of those is on, and not from a session that
signed in with a password, so a working way back is proven first: turn it off while signed in by
SAML (keeping SAML on in the same save), through OpenStare, or with an admin's API token
(`PUT /api/v1/auth/settings`).

### SAML

OpenTrack is the SAML service provider; your identity provider (Keycloak, Entra ID, ADFS…) signs
users in. OpenTrack sends an unsigned request with the HTTP-Redirect binding, and takes the
signed response with the HTTP-POST binding. Sign-on started at the identity provider is not
supported.

1. **Set `OT_PUBLIC_URL`** to the address browsers use, such as `https://opentrack.example.org`,
   and restart. The **Service provider** row in the Single sign-on panel's SAML section then shows the two addresses
   the identity provider needs:
   - entity ID and metadata: `<OT_PUBLIC_URL>/api/v1/auth/saml/metadata`
   - assertion consumer service (ACS): `<OT_PUBLIC_URL>/api/v1/auth/saml/acs`
2. **Register OpenTrack at the identity provider** with those two addresses (or give it the
   metadata URL). It must:
   - put the user's **email address in the NameID** (OpenTrack finds or makes the account by it);
   - **sign** its responses or assertions;
   - accept **unsigned** authentication requests (in Keycloak, turn **Client signature required**
     off);
   - send the user's role in an attribute (in Keycloak, a role list mapper; in Entra ID, app roles,
     sent as `http://schemas.microsoft.com/ws/2008/06/identity/claims/role`).
3. **Paste the identity provider's metadata XML** into **IdP metadata** and choose **Read
   metadata**. The IdP entity ID, sign-in URL and signing certificate below it then show what the
   metadata says. They are read-only: the metadata alone is what OpenTrack checks sign-ons against,
   and the three are read from it again at every save. To change them (after a certificate
   rollover, say), paste the new metadata. Metadata that cannot be read is refused while SAML is
   on.
4. **Role attribute:** the attribute's name (or friendly name), exactly as the provider sends it.
   Default `Role`.
5. **Role mapping:** rows of attribute value → OpenTrack role. Case does not matter. The rows are
   tried in order and **the first match wins**, so put the most specific values first. A user with
   several values gets the role of the first row any of them matches.
6. **Default role:** the role of a user whose values match no row. **None** refuses them.
7. **Allow admin:** off (the default), single sign-on gives at most `track_manager`, whatever the
   mapping says. Turn it on only if the identity provider should make admins.
8. **Button label:** the text of the single sign-on button on the sign-in page. Default "Sign in
   with SSO".
9. Turn **Enabled** on and **Save**. Test it from a private browser window before signing out.

What happens at a sign-on:
- The first time, OpenTrack makes an account (origin `saml`, no password) with the mapped role.
- Next time, a `saml` account takes the role the provider gives it now. If its values map to no
  role and there is no default, it is refused. The identity provider is authoritative for the
  role: Settings → Users shows a `saml` account's role read-only, and the API refuses to change
  it (409). To change it, change the mapping here or the user's role at the provider.
  (`opentrack user role` on the server still can, as a break-glass, until the next sign-on.)
- If the account with that email was made another way (a local account), the sign-on is refused:
  an identity provider never takes over a local account or its role. Use a different email for
  the local account, or delete it so SAML makes a new one.
- A turned-off account is refused.
- Each assertion can be used once, and must answer a request OpenTrack made in the last 5 minutes.

A refused sign-on returns to the sign-in page with "Single sign-on failed". The reason is in the
log (`SAML sign-on refused`, with a `reason`).

The build must include SAML: the Docker image does. A lean build (`--no-default-features`) shows
"Not in this build" instead.

### OpenStare sign-in

When OpenTrack runs beside OpenStare on the same host, users can sign in once. OpenTrack then
accepts:
- a browser signed in to OpenStare on the same host name (its `session` cookie), and
- an OpenStare API token, sent as `Authorization: Bearer`.

For each, OpenTrack asks OpenStare's `/api/auth/me` who it is, and keeps the answer for 30
seconds. So an OpenStare sign-out or revocation holds here within 30 seconds. No OpenTrack
account is made; the decision log records the user's OpenStare email.

In the Single sign-on panel's **OpenStare sign-in** section:
- **API URL:** OpenStare's API as the OpenTrack server reaches it. Default `http://127.0.0.1:3001`.
- **Sign-in page URL:** OpenStare's sign-in page as browsers reach it. It puts a "Sign in with
  OpenStare" button on OpenTrack's sign-in page. Empty: no button.
- **Role mapping:** OpenStare roles (`admin`, `operator`, `analyst`, `guest`) → OpenTrack roles.
  Default: admin → admin, operator → track_manager, analyst → viewer. A role not listed (such as
  `guest`) is refused.

### Client certificates

With mutual TLS, a machine can sign in with its certificate alone.

1. Serve over TLS with a client CA: `OT_TLS_CERT`, `OT_TLS_KEY` and `OT_TLS_CLIENT_CA`.
2. Make an account for the machine (with no password), with the role it needs.
3. In the Single sign-on panel's **Client certificates** section, add a row: the certificate subject's common name (CN), and the
   account's email. Save.

A certificate the CA signed whose CN is listed signs in as that account, while the account is
active. With `OT_TLS_CLIENT_CRL`, revoked certificates are refused; keep the lists current (a list
past its next update refuses every certificate its CA issued), for example with a daily job that
downloads your CA's CRLs into the directory. Refusals are logged as `client certificate refused`. Other callers can still use passwords and tokens.

### Sign-in turned off

`OT_AUTH=off` turns sign-in off: every caller, from anywhere, is an admin, and the decision log
records them as `anonymous`. The server logs a warning at start. Use it only on a development
machine that nothing else can reach. Never in production.

## Banners

### Classification banner

**Settings → Banners.** A fixed bar at the top and bottom of every page, including the sign-in
page:
- **Classification banner:** on or off.
- **Classification text:** up to 128 characters, such as `UNCLASSIFIED // FOR OFFICIAL USE ONLY`.
- **Color preset:** OpenStare's colours for Unclassified, CUI, Confidential, Secret and Top Secret;
  or set **Background color** and **Text color** (`#rrggbb`).

Each OpenTrack sets its own banner, whatever the OpenStare it feeds shows. Browsers pick up a change
within a minute. The banner marks the system; it does not mark the tracks (see
[Security labels](#security-labels)).

### Notice and consent

The **Warning banner** is a notice users must accept after signing in, such as consent to
monitoring, as OpenStare's warning banner:
- **AGREE** goes on; **DECLINE** signs the user out.
- It is asked once per browser tab and account, and again after signing out.
- An admin also sees **Go to Settings**, to fix a wrong text without accepting it.

Turn it on and give the text (up to 20,000 characters). Save.

## Security labels

A source can label everything it reports with a security label, in the fields OpenStare's ICD
reserves: `classification`, `restrictions` and `sharing`. Set it in the source's pipeline, publish
stage, **Security label** (Sources → a source → Pipeline):
- **Classification:** such as `SECRET`.
- **Restrictions:** comma separated, such as `NOFORN, ORCON`.
- **Sharing:** who it may be released to, such as `REL TO USA, FVEY`.

The label goes into each published track message as `security`
([docs/nats-output.md](../nats-output.md)). A track that several labelled sources report for
carries:
- the **highest classification** among them;
- **every restriction** any of them has;
- only the **releasability they all share** (`NONE` when they share none).

Which classification is highest comes from the list in **Settings → Security → Security labels**.
It runs lowest first, and by default reads `UNCLASSIFIED`, `CUI`, `CONFIDENTIAL`, `SECRET`.
- **Reorder:** drag a row by its handle.
- **Add or remove:** **Add classification** adds a row; the × removes one.
- **Save** when you're done.

Case does not matter, `U`, `C`, `S` and `TS` stand for their names, and caveats after `//` are
ignored when ranking. A classification that isn't in the list ranks above all of them, so a track
is never marked too low. Add `TOP SECRET` (or your own levels) if your sources carry them.

## Settings tab

The **Settings** tab, panel by panel, in order. Viewers and track managers see General, Data and
Banners, but can't change them.

| Panel | What it holds | See |
|---|---|---|
| **General** | **Site name** (shown in the header and the browser title; up to 64 characters). The **site code** beside it is `OT_SITE_CODE`, read only. **Position history:** how long each track's positions are kept (hours, 0 to 720; empty 12) and at most one point per track how often (seconds, 0 to 3600; empty 10). Memory is about 130 bytes a point: 2,000 tracks for 12 h every 10 s take about 1.1 GB of Redis. **Plugins:** codec, tracker and scorer plugins. | [Plugins](#plugins) |
| **Users**, **API tokens** | Accounts, with the break-glass ones marked **Never turn off**, and machine tokens (admins only). | [Users](#users) |
| **Nodes** | Sharing the picture with other OpenTrack nodes (admins only). | [Multi-node](#multi-node) |
| **Data** | Export: live tracks as GeoJSON or CSV, the registry as XLSX, and the full configuration (admins only; it holds secrets), with its import while the node is empty. **Purge:** retire every live track. | [Configuration export](#configuration-export), [Purge](#purge) |
| **Banners** | Classification banner and warning banner. | [Banners](#banners) |
| **Security labels** | The classification order a fused track's label is chosen by (admins only). | [Security labels](#security-labels) |
| **Single sign-on** | The sign-in settings, with one Save: password sign-in on or off, SAML, OpenStare sign-in and client certificates (admins only). | [Sign-in](#sign-in) |

The General, Nodes and Banners panels share one draft: **Save** on any of them saves all three.

## Sources and correlation

Configuring what OpenTrack ingests and how it correlates is an admin's job. Operators see the
result on the Sources and Correlation tabs.

### Sources

**Sources → Add source** runs a wizard: transport, framing, codec, then the pipeline designer,
with a live preview of each stage on sample data. Enable a source to start it. Each save is a
revision: the source's **History** tab lists them, with who saved each and the spec it saved. Write secrets as
`${env:NAME}`, resolved when the source starts. Only admins see a source's secrets: for viewers and
track managers, passwords, tokens, header and metadata values, credentials in URLs and API keys in
messages show as `••••••` (`${env:…}` references stay visible). See the README, "Sources and pipelines", and `docs/examples/`.

### Output schema

The **Schema** tab defines the attributes every published track carries. Edit a draft, then
**Publish** it: a published version never changes, and every live track whose attributes change is
republished. A source whose mapping targets an unpublished version doesn't start.

### Correlation settings

The **Correlation** tab's **Settings** panel (admins save; others see it) sets how tracks pair:
identifiers only, kinematics, or kinematics vetoed by metadata; auto or suggest mode; the
kinematic test; splits; and the **output filter**, which holds tracks back by area, affiliation,
domain, track type or confidence. Saved settings apply while the engine runs and override
`OT_CORRELATION`. **Defaults** restores the built-in values. See
[docs/algorithms.md](../algorithms.md).

### Tracker profiles

A source's tracker stage can load a **tracker profile**, a sensor's tracker settings as a JSON file.
Profiles shipped with OpenTrack are read from `OT_PROFILES_DIR`; profiles imported or saved in the
pipeline designer go to `profiles/trackers/` beside the database. Back that directory up with the
database.

## Plugins

Codecs, trackers and pairing scorers of your own, beside the built-in ones. **Settings → General → Plugins**
(admins change it; others see it):
- **Add plugin:** a WebAssembly component (`.wasm`, run sandboxed inside OpenTrack), or an
  external plugin's address (a program of its own serving the plugin interface on a socket).
  **Replace** installs a new build of a plugin with the same name, keeping its grants and whether
  it is enabled.
- **Enable** toggle, **Check** (loads it and opens each kind it provides), **Grants**, **Delete**.
- **Grants** (WebAssembly only): memory ceiling (MB), time per call (ms; a call that runs longer is
  stopped), network addresses it may connect to, server directories it may see (read only unless
  `rw`), and environment variables.
- **Used by** shows the sources and correlation settings that use it.

A WebAssembly plugin is stored in the database, so it is backed up with it. An external plugin is
only an address: run and back up its program yourself.

Plugins are loaded without a restart. From the command line: [Plugin commands](#plugin-commands).
Writing plugins: [docs/plugins.md](../plugins.md).

## Multi-node

Several OpenTrack nodes (server sites, or a drone swarm) can share one track picture with no node
in charge: one track number per object on every node, the node that sees a track best reports it,
and a track manager's decision on any node holds on all of them. OpenTrack does not do the
networking: the `link` role puts sync messages on this node's NATS (`ot.sync.out.*`,
`ot.sync.in.*`), and a networking package, or `opentrack bridge` between server sites, carries
them. Design: [docs/multi-node.md](../multi-node.md). Message format:
[docs/sync-icd.md](../sync-icd.md). Two-site example: `docs/examples/two-sites/`.

To turn it on:
1. Give every node its own `OT_SITE_CODE`, and run the `link` role (`all` includes it).
2. Carry `<OT_SYNC_PREFIX>.out.*` to the other nodes' `<OT_SYNC_PREFIX>.in.*`.
3. In **Settings → Nodes**:
   - **Share the picture:** on.
   - **Trusted nodes:** the other nodes' site codes, comma separated. Messages from any other node
     are dropped.
   - **Sending budget:** this node's share of the link in kbit/s (0: no cap). Past it, new tracks,
     state changes and the largest drifts go first.
   - **Receive only:** apply other nodes' track management here but accept none from this node's
     users (a node with no operator, such as a drone).
   - **Share the profile:** publishing an output schema or saving correlation settings on any node
     applies on every node that shares the profile; the later change wins.
4. Save. The table shows each trusted node: heard, silent (not heard for 30 s) or never heard, and
   how many of its decisions this node holds. Below it: how many tracks this node reports and holds
   only from others, and the decisions applied, waiting for their tracks, superseded or failed.

The registry, sources and plugins are not shared between nodes.

## Backup and restore

### What to back up

| What | Why | How |
|---|---|---|
| `opentrack.db` | Everything configured and decided. | [SQLite backup](#sqlite-backup), online. |
| `session.key` (or your `OT_SESSION_SECRET`) | Without it, every session and API token is void. | Copy it; keep it secret. |
| `profiles/trackers/` beside the database | Imported tracker profiles. | Copy it. |
| `.env` or wherever the `OT_*` settings live | The deployment's settings. | Copy it; it holds secrets. |
| Redis | The live picture and its track numbers. Optional, but see [Restore](#restore). | [Redis backup](#redis-backup). |
| External plugins | Only their address is in the database. | Back up their programs. |
| A [configuration export](#configuration-export) (optional) | The configuration in one file, to rebuild a node from. Not the track state or the audit record. | `opentrack config export --out <file>`; keep it as safe as the database. |

### SQLite backup

The database is in WAL mode, so a plain copy of `opentrack.db` while OpenTrack runs can miss
changes still in `opentrack.db-wal`, or be torn. Back it up online with SQLite itself, on the same
host (not over a network file system):

```sh
sqlite3 data/opentrack.db ".backup '/backups/opentrack-$(date +%Y%m%d-%H%M).db'"
```

or, with SQLite 3.27 or newer, a compacted copy:

```sh
sqlite3 data/opentrack.db "VACUUM INTO '/backups/opentrack-$(date +%Y%m%d-%H%M).db'"
```

Both take a consistent snapshot while OpenTrack goes on writing. The Docker image has no `sqlite3`;
run it on the host, against the mounted data directory. Check a backup with
`sqlite3 <backup> "PRAGMA integrity_check"`. Or stop OpenTrack and copy the three files
(`opentrack.db`, `-wal`, `-shm`) together.

### Redis backup

Redis persists itself: take a snapshot with `redis-cli BGSAVE` and copy its `dump.rdb` once
`LASTSAVE` changes, or run Redis with append-only files. That backs up the whole Redis, other
applications' keys included. Take it at the same time as the SQLite backup.

### Configuration export

**Settings → Data → Full configuration** (admins; `GET /api/v1/export/config`, or
`opentrack config export`) downloads the node's whole configuration as one JSON file, to back it
up or to [rebuild the node](#rebuild-a-node-from-a-configuration-export) from. Every export is
recorded (an `export_config` decision in the audit record, with who exported it and how much), and
the download is an attachment no cache keeps (`Cache-Control: no-store`).

**It holds secrets.** Source credentials, password hashes, the SAML and OpenStare settings and
plugin components are in it as stored. Keep it as safe as the database: an encrypted volume or a
vault, readable by admins only, and delete copies you no longer need. `opentrack config export
--out <file>` writes a file only its owner can read.

What it holds (the file's sections):

| Section | What |
|---|---|
| `format`, `version` | `"opentrack-config"`, `2`. Also `opentrack` (the release that wrote it), `site_code`, `exported_at`, `exported_by`, `notice`. |
| `sources` | Every source as stored: `id`, `name`, `transport`, `codec`, `enabled`, `priority`, `revision`, `spec` (with its secrets), `raw_subject` (raw output, if consented). |
| `schema_versions` | Every output schema version (`version`, `status`, `published_at_ms`, `notes`, `fields`), the draft too. |
| `correlation_settings` | The saved correlation settings, security labels included (`null`: never saved). |
| `app_settings` | The instance settings as saved (site name, banners, position history, nodes), `{}` if never saved. |
| `auth_settings` | The sign-in settings as saved: password sign-in, SAML, OpenStare trust, client certificate mappings, and the accounts inactivity never turns off. A file from 0.4.0 may also have password, lockout, session, inactivity-period and audit-retention settings: they import, and are ignored (those values are fixed). |
| `accounts` | Every account: `id`, `email`, `name`, `role`, `active`, `origin`, `password_hash`, `password_history`, `password_changed_at_ms`, `must_change_password`, `locked_until_ms`, `last_login_at_ms` and the other account dates. |
| `api_tokens` | Every API token's record: `jti`, `name`, `user_id`, `created_by`, `expires_at_ms`, `revoked_at_ms`. Not the tokens themselves, which are never stored. |
| `registry.entities` | Every entity as the registry API shapes it: identifiers, status, publish override, the OTH-GOLD minimum and attributes. |
| `plugins` | Every added plugin: `name`, `runtime`, `version`, `manifest`, `wasm` (the component, base64) and `sha256`, or `address` (external), `grants`, `enabled`. |
| `tracker_profiles` | The tracker profiles imported on this node (not the shipped ones), each as its file. |
| `uid_sequences` | The next track number per site code (`site`, `next_sequence`). |

What it never holds: the session signing key (`session.key` or `OT_SESSION_SECRET`), sessions,
the audit record, the decision log, the track graph, groups, correlation suggestions, entity and
source revision history, anything in Redis, and the `OT_*` deployment settings. API tokens are
signed with the session key, so the tokens in the file work again only on a node with the same
key. External plugins' programs are not in it (only their address).

Files from 0.4.0 and earlier (no `format`, version 1) held only sources, schema versions and
correlation and instance settings; they cannot be imported.

### Rebuild a node from a configuration export

An import rebuilds a node that has **no configuration yet**, and refuses any other, listing what is
there. A node is empty when it has:
- no sources, registry entities, plugins, API tokens or imported tracker profiles;
- no output schema version but the built-in version 1;
- no saved correlation, instance or sign-in settings (the defaults);
- at most one account, an admin: the first admin made on first start.
- never imported a configuration before.

Track state (tracks, other decisions, the audit record) does not count. On a new node:

```sh
opentrack config status                     # empty?
opentrack config import opentrack-config-OTK-20260929-1200.json
opentrack serve                              # or all; restart a running server
```

The CLI is the usual way: an empty node's only account is its first admin. From the UI, an admin
of an empty node (after changing the first admin's password) sees **Settings → Data →
Import configuration**; `POST /api/v1/import/config` with the file as the body does the same, and
`GET /api/v1/import/config` says whether the node is empty (`{"empty", "present"}`).

The import checks every section first, as the API checks it when saved: sources against the file's
output schema versions, each settings document by its own rules, accounts (unique emails, valid
roles, at least one active admin), tokens against the accounts, entities and their identifiers,
plugins (the component's SHA-256 must match), tracker profiles, track numbers. A malformed file,
another format or version, or any invalid section is refused with every problem listed, and
nothing is written. Then it writes everything in one SQLite transaction (tracker profile files
included, removed again if the transaction fails), recorded as one `import_config` decision.

- **Accounts:** the imported accounts replace the first admin (whatever its email), with their
  password hashes, history and flags, so everyone signs in with their old password.
  `initial-admin.txt` is deleted. A file with no accounts (from a node with sign-in off) keeps the
  first admin.
- **API tokens:** set the same `OT_SESSION_SECRET` (or copy `session.key`) for them to work;
  otherwise revoke them and issue new ones.
- **Raw output** of a source is consented again by the import decision.
- **Track numbers** continue from the file's counter (or this node's, if higher), so no number is
  issued twice. Keep the same `OT_SITE_CODE`; the import notes it if the file came from another
  site.
- Restart a server that was running during a CLI import, so it reads the new sign-in settings.

### Registry export

**Registry → Export XLSX** or **Export CSV** (any role; `GET /api/v1/registry/export?format=xlsx|csv`)
downloads every entity: id, name, status, publish override, the OTH-GOLD minimum, identifiers and
attributes. **Import sheet** (track managers) brings a sheet back: it plans every row first, and
writes nothing while any row has an error. A row updates the entity its `entity_id` names (and
creates it under that id if missing), else the one its identifiers belong to, else creates one.
The entities' revision history is not in the sheet.

### Restore

1. **Stop every OpenTrack role.**
2. **Put the database back:** copy the backup to `OT_SQLITE_PATH`, and delete any
   `opentrack.db-wal` and `opentrack.db-shm` left from the old database. Put back `session.key`
   (or set the same `OT_SESSION_SECRET`) and `profiles/trackers/`.
3. **Redis:** restore the Redis snapshot taken with the database backup. If you have none, clear
   OpenTrack's keys, so the engine does not start from a picture the database doesn't know:
   ```sh
   redis-cli --scan --pattern 'tms:*' | xargs -r -n 500 redis-cli del
   ```
   (use your `OT_REDIS_NAMESPACE`). Other applications' keys stay.
4. **Track numbers are checked on start.** Track numbers come from a counter in the database, so
   a backup takes the counter back to its value then. Before issuing any number, the engine looks
   for the highest number of its site code already in use: in the database (track graph, decision
   log), in Redis (live tracks and their history) and in the NATS tracks stream (its subjects,
   `<prefix>.tms-<UID>`). If the counter is not past it, the engine moves it on (never back), logs
   a warning with the old and new values, and records a `uid_counter_advanced` decision saying
   where the number was found. If NATS cannot be reached then, the engine logs that it could not
   check the stream and relies on the database and Redis: start NATS first when restoring.
   Tracks this node published that are no longer live (Redis lost or cleared) would otherwise stay
   in the stream as consumers last saw them: the engine publishes a delete for each (and logs how
   many). Subjects of other site codes, or last written by another publisher, are left
   alone and age out (`OT_NATS_MAX_AGE_HOURS`).
5. **Start OpenTrack.** Migrations run if the backup is from an older release. Check the Overview
   tab's **System status**.

## Upgrades

1. **Read [CHANGELOG.md](../../CHANGELOG.md)** from your release to the new one. Each release lists
   what changed and, under **Upgrading**, what to do.
2. **Back up** the database and `session.key` ([SQLite backup](#sqlite-backup)).
3. **Stop OpenTrack, install the new release** (`docker compose up -d --build`, or the new binary
   and UI build), and start it. Database migrations run on start. To run them on their own first:
   `opentrack migrate`, which prints the new schema version.
4. **Check** the Overview tab: **System status** shows the version and the SQLite schema version,
   and every dependency should be green. Browsers load the new UI on their next page load.

With roles in separate processes, migrate once (or start one role) before starting the others.

To go back, restore the backup from step 2 and run the old release. An older release refuses a
database a newer one has migrated: "database schema version N is newer than this build supports
(M)".

OpenTrack is alpha: the published message, the plugin interface and the database schema may still
change between releases.

## Purge

**Settings → Data → Purge** (admins; `POST /api/v1/admin/purge`) clears the picture:
1. Select **Purge tracks**.
2. In the window that opens, optionally turn on **Also delete history**, then select **Purge**.

It then:
- dissolves every group;
- retires every live track, and deletes the published ones downstream;
- with **Also delete history**, also deletes the track graph (how every track was formed, paired,
  merged and split, and the do-not-pair rules between tracks) and every correlation suggestion.

It keeps sources, the output schema, the registry and its entities, settings, accounts, plugins
and the decision log (a purge is itself a decision). Sources keep reporting, so new tracks appear
at once, with new numbers. It cannot be undone.

The API call is `{"confirm": "<site code>", "history": true|false}`: the API still asks for the
site code, so a stray request can't purge. It waits up to 5 minutes for the engine.

## Monitoring

### Health checks

- **`GET /healthz`** answers `ok`, with no sign-in. It only says the control plane process
  answers. Use it for a container or load-balancer liveness check.
- **`GET /api/v1/status`** (any role; use a `viewer` API token) reports each dependency with an
  `ok` flag: `sqlite` (with its schema version), `redis`, and `nats` (connected, and the stream
  usable), plus the version, algorithm versions, site code and node id. It answers 200 when all
  three are up and 503, with the same body, when any is down, so a readiness check or monitor
  that reads only the status code sees it; the flags say which.
- **`GET /api/v1/sync/status`**: the link to other nodes.

### Metrics

The **Overview** tab:
- **System status:** control plane, algorithms, SQLite, Redis and NATS, each green or red with
  its error.
- **Throughput** (per minute, last hour): ingest (frames, observations, dropped, errors),
  observations by source, correlation (applied, new tracks, paired, proposed or split, ended), and
  output (tracks written, deletes, raw feed, errors).
- **Tracks:** live, confirmed, tentative, lost, with an entity, entity differs, and per domain.
- **Backlog and resources:** CPU, memory, threads, Redis, SQLite and NATS stream size, the
  engine's and writer's backlogs, uptime.

A backlog that keeps growing means the engine or the writer can't keep up (or isn't running). The
engine can fall behind by at most `OT_OBS_WINDOW_SECS` before reports are lost.

The same figures come from `GET /api/v1/metrics?minutes=60` (up to 1440), and a source's from
`GET /api/v1/sources/<id>/metrics`. Each source's **Status** tab shows its state, last frame and
error.

### Logs

OpenTrack logs to standard output (`docker compose logs -f opentrack`). Set `OT_LOG_FORMAT=json`
for a log collector, and `OT_LOG` for more or less detail ([Logging](#logging)). Worth watching:
- `audit record` (target `audit`): every audit row, see [Audit record](#audit-record);
- `SAML sign-on refused` and `sign-in refused`;
- `OpenStare sign-in check failed`;
- a source's connection errors;
- `the engine did not answer`.

Every change a person makes is in the decision log, with who and when (the Track Management and
Correlation tabs show it; `GET /api/v1/decisions`).

## Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| The server doesn't start: `OT_ADMIN_PASSWORD: a password needs at least 8 characters`. | Use a longer password, or leave both `OT_ADMIN_*` unset. |
| No `initial-admin.txt`. | It is made only on the first start with no accounts and no `OT_ADMIN_*`. Use `opentrack user add <email> --role admin --password-stdin`. |
| Every admin is locked out. | `opentrack user passwd <email>` (or `user enable`, or `user add … --role admin`) on the server. |
| Everyone was signed out after a restart, and API tokens fail. | The signing key changed: `session.key` was lost or `OT_SESSION_SECRET` changed. Restore it, or make new tokens. |
| "Too many attempts". | 5 sign-in attempts at once per address, then one a second. Wait. |
| "Single sign-on failed". | Read `SAML sign-on refused` in the log: no `OT_PUBLIC_URL`, a response not signed or with the wrong certificate (paste fresh metadata), no email in the NameID, no role mapped and no default, the account is off, or more than 5 minutes between request and answer. |
| "sign in with single sign-on before turning password sign-in off". | See [Password sign-in](#password-sign-in). |
| An action fails with "the engine did not answer within 10 s (is it running?)". | The engine isn't running (a lone `serve`), or is stuck: check its log and the engine backlog. |
| "database schema version N is newer than this build supports". | A newer release migrated this database. Run that release, or restore the pre-upgrade backup ([Upgrades](#upgrades)). |
| NATS is red in System status. | Not connected (check `OT_NATS_URL` and credentials), or the stream is unusable: its error is shown. OpenTrack creates the stream only if it is missing. |
| A source stays "connecting" or "failing". | Its **Status** tab shows the error. Check the address, TLS files and `${env:…}` secrets. |
| Tracks are live but consumers don't get them. | See why each is not published on its track card (operator guide, "Not published"): not confirmed, only sensors that may not stand alone report for it, the output filter holds it, or an entity says never. |
| `docker compose up` refuses to start. | `.env` is missing next to `docker-compose.yml`. |
| The container exits at once. | The data directory isn't writable by uid 1000 (see its log). |
| Redis memory keeps growing. | Position history (Settings → General), `OT_OBS_WINDOW_SECS`, and the number of tracks. See [Redis](#redis). |

## Security

- **Keep sign-in on.** `OT_AUTH=off` makes everyone an admin.
- **Use TLS** (`OT_TLS_CERT`, `OT_TLS_KEY`), or a TLS-terminating proxy that forwards to OpenTrack
  on a private address. Over TLS, session cookies are `Secure`.
- **Protect the data directory.** `session.key` and `initial-admin.txt` are written readable by
  their owner only; the database holds password hashes and plugin files. Keep backups as
  protected.
- **Keep secrets out of source specs.** Use `${env:NAME}`. Non-admins see secret fields hidden,
  but admins, the database and its backups hold whatever is written inline.
- **Give the least role.** Viewers for monitoring and service accounts that only read; admins
  few. Leave **Allow admin** off for SAML unless the identity provider should make admins.
- **Local accounts and SAML:** SAML never signs in to a local account: a sign-on whose email matches
  one is refused. Keep local accounts for break-glass and service use.
- **Rotate API tokens**: give them an expiry, one per service, and revoke the ones not used.
- **Everything is recorded.** The decision log names the account behind every change: accounts,
  sign-in settings, sources, schema, settings and track management.

<a id="security-hardening"></a>

### Security hardening (0.4.0)

0.4.0 is the accreditation release, aimed at DoD RMF with the ASD STIG and NIST 800-53 Moderate.
Each control below is always on, at the STIG value: the password policy, lockout, session limits,
inactivity and audit retention are fixed, not settings. What an admin sets is the break-glass
accounts (**Never turn off** in Settings → Users) and sign-in itself (the **Single sign-on**
panel). How each one maps to a control is in
[docs/security/stig-mapping.md](../security/stig-mapping.md). Deployment steps are in
[docs/security/hardening.md](../security/hardening.md).

### FIPS cryptography

All of OpenTrack's cryptography runs in FIPS 140-3 validated modules. That covers TLS, session and
token signatures, password hashes and random secrets (the AWS-LC FIPS module), and SAML
signatures (OpenSSL's 3.0.9 FIPS provider, in the image). The process stops at start if AWS-LC
is not in FIPS mode. SAML sign-in is off wherever OpenSSL is not, which means everywhere outside
the image. Details: [docs/security/fips.md](../security/fips.md).

### Password policy

For local accounts:

| Rule | Value |
|---|---|
| Minimum length | 15 |
| Upper case, lower case, digit, special character | all required |
| Characters that must change from the last password | 8 |
| Earlier passwords that can't be reused | 5 |
| Minimum age (between changes) | 24 hours (an admin's reset is exempt) |
| Maximum age | 60 days |

An expired password, or a temporary one an admin set, must be changed at the next sign-in. Until
it is, that session can do nothing else. The first admin's password is temporary as well. That
includes the one in `initial-admin.txt`, and `OT_ADMIN_PASSWORD` must already meet the policy.

### Account lockout

Three failed sign-ins within 15 minutes lock an account for 15 minutes. Every refusal gives the same answer, "wrong email or
password", whether the account is unknown, turned off, locked or the password is wrong. The
audit record keeps the real reason. To unlock an account sooner:
- **Settings → Users:** the open-lock button on its row;
- **command line:** `opentrack user unlock <email>`;
- **API:** `POST /api/v1/auth/users/{id}/unlock`.

### Session limits

- **Idle timeout:** 15 minutes, or 10 for admins. The page's own refreshing doesn't count as use;
  only what the user does.
- **Absolute lifetime:** 24 hours, used or not.
- **Sessions per account:** 3. A fourth sign-in ends the oldest.

Users see and end their own sessions from the account menu, under **Sessions**. An admin ends all
of an account's sessions with **Sign out everywhere** on its row in Settings → Users
([Revoking sessions](#revoking-sessions)). Sessions are kept per node. API tokens aren't
sessions: they have no idle timeout, only their expiry.

### Inactive accounts

Accounts that haven't signed in for 35 days are turned off. The check runs at sign-in and every
10 minutes. Re-enabling an account restarts its clock. Mark break-glass accounts with **Never
turn off** on their row in Settings → Users (keep them few, with sealed passwords). Otherwise a
sole admin who doesn't sign in for 35 days is turned off too, and only
`opentrack user enable <email>` on the server brings the account back.

After each sign-in, users see when they last signed in and how many failed attempts there were
since then.

### Audit record

Every sign-in event is written to an append-only `audit` table, along with every decision the
decision log records:
- sign-in succeeded or refused, with the reason and address;
- sign-out, lockout, unlock, session ended or timed out;
- password changed, account turned off.

Each row carries a SHA-256 over the row before it, so a row changed, removed or inserted breaks
the chain. The database refuses updates to the table.
- **Review in the server logs:** every row is also logged, once written, as an `audit record`
  event (target `audit`) with its `seq`, `actor`, `op`, `outcome`, `ip`, `detail`, `decision_id`
  and `hash`. Set `OT_LOG_FORMAT=json` and ship the log to your SIEM. A decision's `before` and
  `after` (the configuration it changed, which can hold source credentials) are left out of the
  log; its `decision_id` finds them in the API. For example:

  ```json
  {"timestamp":"2026-09-29T14:02:11.418220Z","level":"INFO","fields":{"message":"audit record","seq":4182,"actor":"ann@example.org","op":"login","outcome":"failure","ip":"10.1.2.3","detail":"{\"consecutive\":2,\"reason\":\"bad_password\",\"via\":\"password\"}","hash":"55e122e2f66b7312482b6dd6624afe22e3c4f0d6f1b44c29cb1333c1a32c5d84"},"target":"audit"}
  ```
- **Review through the API** (admins): `GET /api/v1/audit` filters by time (`from_ms`, `to_ms`),
  account (`actor`), event (`op`, comma-separated) and `outcome`, pages back with `before_seq`,
  and gives CSV with `format=csv`. `GET /api/v1/audit/verify` checks the chain.
- **Fail closed:** if a sign-in can't be recorded, it is refused.
- **Retention:** kept forever. Nothing deletes rows.
- **Chain head:** written to the log every hour. Keep the logs apart from the database, so that a
  truncated tail can be spotted.

### Web protections

Every response carries:
- a Content Security Policy (this origin only, no framing);
- `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer` and a
  minimal `Permissions-Policy`;
- HSTS over TLS;
- `Cache-Control: no-store` on the API.

The session cookie is `HttpOnly`, `SameSite=Strict`, and `Secure` whenever TLS is in use,
including TLS ended at a proxy (`OT_PUBLIC_TLS=1` or an `https://` `OT_PUBLIC_URL`).

### Encryption in transit

| Link | How |
|---|---|
| Browsers and API clients | `OT_TLS_CERT`/`OT_TLS_KEY`, optional client certificates (`OT_TLS_CLIENT_CA`, revocation lists `OT_TLS_CLIENT_CRL`), or a TLS proxy |
| NATS | a `tls://` URL, `OT_NATS_CA`, and mutual TLS with `OT_NATS_CERT`/`OT_NATS_KEY` |
| Redis | a `rediss://` URL, `OT_REDIS_CA`, and mutual TLS with `OT_REDIS_CERT`/`OT_REDIS_KEY` |
| Feeds | per source, in its transport's TLS settings |

### Container hardening

The image runs as an unprivileged user and has a health check (`opentrack health`). Its base
images are pinned by digest. The compose file runs it with:
- a read-only root file system (only `/data` and scratch space are writable);
- every Linux capability dropped;
- `no-new-privileges`.

### Signed images

Release images (`ghcr.io/phornstein/opentrack:<version>`) are signed with the project's cosign
key and carry SLSA provenance and an SBOM. Check one before you run it:

```sh
cosign verify --key cosign.pub --insecure-ignore-tlog=true ghcr.io/phornstein/opentrack@<digest>
```

`cosign.pub` is in the repository; the release notes list each image's digest.
`--insecure-ignore-tlog` is expected: the signatures go to no public transparency log.

### Upgrading to 0.4.0

- **Everyone signs in again:** sessions from before the upgrade have no server-side record.
- **Password age** counts from the upgrade, so existing passwords expire 60 days later. They also
  must meet the policy at their next change.
- **Password hashes** move from Argon2id to PBKDF2 at each account's next sign-in.
- **SAML** works only in the image, where OpenSSL has its FIPS provider, and no longer signs in to
  local accounts: a user with both needs a different email for the local one.
- **Nodes that share their profile** must all run 0.4.0. Older nodes refuse correlation settings
  that carry the classification order.
