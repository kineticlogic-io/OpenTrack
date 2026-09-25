# Consuming OpenTrack system tracks over NATS

OpenTrack publishes every system track to a NATS JetStream stream. This is the whole contract a
consumer (OpenStare) needs.

## Stream and subjects

| | Default | Setting |
|-|---------|---------|
| Stream | `TRACKS` | `OT_NATS_STREAM` |
| Subjects | `tracks.>` | `OT_NATS_TRACKS_SUBJECT` (the prefix) |
| One track | `tracks.tms-OTK000000123` | |
| Messages kept | 1 per subject (the latest) | |
| Max age | 24 h without a newer message | `OT_NATS_MAX_AGE_HOURS` |
| Duplicate window | 2 min | |

OpenTrack creates the stream with these settings if it does not exist, and never changes an
existing one, so an operator may create it beforehand (for example with different storage or
replicas). It must capture `tracks.>` and should keep one message per subject.

Because the stream keeps the latest message per track, a consumer that starts late gets the whole
current picture by reading the stream from the start (for example an ordered consumer with
`DeliverPolicy::LastPerSubject` on `tracks.>`), then keeps receiving updates. Live tracks are
republished whenever they change, at most every 5 s each (immediately when identity or
classification changes); a track with no report for 6 h is dropped and deleted.

## Headers

| Header | Value |
|--------|-------|
| `OT-Op` | `upsert` or `delete` |
| `OT-Schema` | `opentrack.track.v1` |
| `Nats-Msg-Id` | unique per update, e.g. `opentrack-OTK:1790303694426-0` |

## `upsert`

The full current state of one track; it replaces whatever the consumer holds for that
`track_id`. Timestamps are RFC 3339 UTC, units are SI (metres, metres per second, degrees true),
enums are lower-case strings, and absent values are omitted rather than sent as null.

```json
{
  "schema": "opentrack.track.v1",
  "op": "upsert",
  "track_id": "tms-OTK000000003",
  "uid": "OTK000000003",
  "state": "confirmed",
  "classification": { "cot_type": "a-p-S", "domain": "surface", "affiliation": "pending" },
  "identity": {
    "name": "OPENTRACK SYNTHETIC SYN-1790303764-01",
    "identifiers": [{ "scheme": "synthetic", "value": "SYN-1790303764-01" }]
  },
  "position": { "lat": 32.7, "lon": -117.25 },
  "kinematics": { "course_deg": 270.0, "speed_mps": 5.0, "heading_deg": 270.0 },
  "confidence": 1.0,
  "observed_at": "2026-09-25T02:36:04.662565816Z",
  "first_seen": "2026-09-25T02:36:04.662565816Z",
  "last_seen": "2026-09-25T02:36:04.662565816Z",
  "observation_count": 1,
  "contributors": [
    {
      "source_id": "synthetic",
      "source_track_key": "SYN-1790303764-01",
      "pairing": "auto",
      "confidence": 1.0,
      "last_report": "2026-09-25T02:36:04.662565816Z"
    }
  ],
  "ext_schema_version": 1,
  "publisher": { "node_id": "opentrack-OTK", "version": "0.1.0" },
  "published_at": "2026-09-25T02:36:09.752308997Z"
}
```

| Field | |
|-------|-|
| `track_id` | `tms-<UID>`: the key. Stable for the life of the track. |
| `state` | `tentative`, `confirmed`, `lost` or `dropped` |
| `classification.cot_type` | CoT type with the affiliation atom applied |
| `classification.domain` | `air`, `surface`, `subsurface`, `ground` or `space` |
| `classification.affiliation` | `pending`, `unknown`, `assumed_friend`, `friend`, `neutral`, `suspect`, `hostile`, `joker`, `faker` or `none` |
| `identity.identifiers` | every identifier, each with its scheme (`mmsi`, `imo`, `icao`, `elnot`, ...) |
| `position.alt_hae_m` | height above the WGS84 ellipsoid, when known |
| `uncertainty` | `cep_m`, `ellipse` (`semi_major_m`, `semi_minor_m`, `orientation_deg`), `vertical_error_m` |
| `kinematics` | `course_deg`, `speed_mps`, `heading_deg`, `vertical_rate_mps` |
| `platform` | `type_code`, `class`, `name`, `flag`, `hull` |
| `contributors` | the source tracks reporting for this track |
| `field_sources` | which contributor supplied each field group |
| `aliases` | track ids merged into this one; treat them as this track |
| `groups` | operator-defined groups |
| `ext`, `ext_schema_version` | admin-defined extension fields and the schema version they follow |

## `delete`

The track was retired (dropped after going stale, merged away, or deleted by an operator). The
consumer removes it. The delete stays in the stream as that subject's latest message until it
ages out, so a consumer that starts later still learns of it.

```json
{
  "schema": "opentrack.track.v1",
  "op": "delete",
  "track_id": "tms-OTK000000001",
  "uid": "OTK000000001",
  "reason": "no report for 21600s",
  "deleted_at": "2026-09-25T02:34:54.426550476Z",
  "publisher": { "node_id": "opentrack-OTK", "version": "0.1.0" }
}
```

## Other subjects

Nothing else OpenTrack publishes falls under `tracks.>`. Per-source raw output, when an admin
enables it with recorded consent, goes to `opentrack.raw.<source>` (or an admin-chosen subject
outside `tracks.>`).

## Permissions OpenTrack needs

Publish on `tracks.>` and on any raw-output subjects, plus the JetStream API for the stream
(`$JS.API.STREAM.INFO.TRACKS`, and `$JS.API.STREAM.CREATE.TRACKS` unless the stream is created
beforehand). Authenticate with `OT_NATS_CREDS` (credentials file), `OT_NATS_TOKEN`, or
`OT_NATS_USER` and `OT_NATS_PASSWORD`.
