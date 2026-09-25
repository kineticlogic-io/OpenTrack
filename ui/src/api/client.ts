// Typed client for the OpenTrack control plane (`/api/v1`).

export interface DependencyStatus {
  ok: boolean
  error?: string
  [key: string]: unknown
}

export interface ServerStatus {
  service: string
  version: string
  site: string
  node_id: string
  sqlite: DependencyStatus & { schema_version?: number; path?: string }
  redis: DependencyStatus & { namespace?: string }
  nats: DependencyStatus & {
    url?: string
    connected?: boolean
    server_name?: string
    server_version?: string
    stream?: string
    tracks_subject?: string
    stream_messages?: number
    stream_bytes?: number
  }
}

export interface Observation {
  source_id: string
  source_track_key: string
  name?: string
  callsign?: string
  observed_at: string
  position: { latitude: number; longitude: number; altitude_hae_m?: number }
  kinematics: { course_deg?: number; speed_mps?: number; heading_deg?: number }
  classification: { cot_type?: string; domain?: string; affiliation?: string }
  identifiers?: { scheme: string; value: string }[]
}

/** A card value that differs from what a feed reports; the card value is published. */
export interface AttributeNotice {
  key: string
  card: unknown
  feed: unknown
  source_id: string
}

export interface SystemTrack {
  uid: string
  state: 'tentative' | 'confirmed' | 'lost' | 'dropped'
  view: Observation
  /** The entity (card) this track resolves to. */
  entity_id?: string
  /** Published attributes, resolved from the card, the feed and built-ins. */
  attributes?: Record<string, unknown>
  notices?: AttributeNotice[]
  contributors: {
    source_id: string
    source_track_key: string
    pairing: 'auto' | 'manual'
    confidence: number
    last_report: string
  }[]
  first_seen: string
  last_seen: string
  observation_count: number
}

export interface TrackResponse {
  track: SystemTrack
  /** The `opentrack.track.v2` message as published. */
  message: TrackMessage
  subject: string
}

/** OTH-GOLD minimum plus the output schema's attributes. */
export interface TrackMessage {
  track_id: string
  class: string
  name: string
  domain: string
  affiliation: string
  force_code: number
  track_type: string
  /** Symbol identification code and its standard. */
  sidc: { standard: '2525c' | '2525d' | 'cot'; code: string }
  time: string
  lat: number
  lon: number
  attributes?: Record<string, unknown>
}

export interface GraphEdge {
  id: number
  kind: string
  src_key: string
  dst_key: string
  attrs: Record<string, unknown>
  valid_from_ms: number
  valid_to_ms: number | null
  decision_id: number
  decision_op: string
  decision_actor: string
  ended_by: number | null
}

/** A row of the live tracks list: GOLD fields as published plus what the table shows. */
export interface TrackRow {
  uid: string
  track_id: string
  entity_id: string | null
  /** Card values that differ from what a feed reports. */
  notices: number
  state: SystemTrack['state']
  class: string
  gold_name: string
  domain: string
  affiliation: string
  force_code: number
  track_type: string
  sidc: TrackMessage['sidc']
  name?: string | null
  callsign?: string | null
  identifiers?: { scheme: string; value: string }[]
  latitude: number
  longitude: number
  course_deg?: number | null
  speed_mps?: number | null
  last_seen: string
  observation_count: number
  sources: string[]
}

export interface Backlog {
  pending: number
  lag: number
}

/** `/metrics`: per-minute counters and gauges, plus live values. */
export interface SystemMetrics {
  minutes: number
  recent_minutes: number
  series: {
    /** Unix minutes. */
    minute: number
    /** Pipeline stage counters summed over sources. */
    ingest: Record<string, number>
    emitted_by_source: Record<string, number>
    engine: Record<string, number>
    writer: Record<string, number>
    /** Gauges sampled by the server (last value in the minute). */
    system: Record<string, number>
  }[]
  /** Totals over the last `recent_minutes`. */
  recent: {
    sources: Record<string, Record<string, number>>
    engine: Record<string, number>
    writer: Record<string, number>
  }
  live: {
    tracks: number
    by_state: Record<string, number>
    by_domain: Record<string, number>
    with_card: number
    notices: number
    outbox: Backlog
    observations: Backlog
    redis_bytes: number
    rss_bytes: number
    threads: number
    nats_messages: number | null
    nats_bytes: number | null
    sqlite_bytes: number
    uptime_secs: number
    cpu_milli: number | null
  }
}

export class ApiError extends Error {
  readonly status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

/** Who the API records as the acting operator until authentication exists. */
const ACTOR = 'op:ui'

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api/v1${path}`, {
    method,
    headers: {
      'x-opentrack-actor': ACTOR,
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const text = await res.text()
  const parsed = text ? JSON.parse(text) : {}
  if (!res.ok) throw new ApiError(res.status, parsed.error ?? res.statusText)
  return parsed as T
}

const get = <T,>(path: string) => request<T>('GET', path)
const enc = encodeURIComponent

// --- Sources -------------------------------------------------------------------------------

/** A source spec as stored: transport + pipeline, all JSON. */
export interface SourceSpec {
  id: string
  name: string
  description?: string
  transport: { type: string; [k: string]: unknown }
  pipeline: {
    codec: { type: string; [k: string]: unknown }
    mapping: { schema_version?: number; rules: unknown[]; reject?: unknown[] }
    [k: string]: unknown
  }
  priority?: number
}

export interface LinkStatus {
  connected: boolean
  connects: number
  errors: number
  last_error?: string
  last_frame_at?: string
}

export interface SourceStatus {
  source: string
  revision: number
  transport: string
  link: LinkStatus
  last_error: string | null
  totals_since_start: Record<string, number>
  updated_at: string
}

export interface SourceRow {
  id: string
  name: string
  transport: string
  codec: string
  enabled: boolean
  priority: number
  revision: number
  spec: SourceSpec
  raw_subject: string | null
  created_at_ms: number
  updated_at_ms: number
  /** Live worker status; null when no worker runs it (disabled or stopped). */
  status: SourceStatus | null
}

export interface MetricsResponse {
  source: string
  minutes: number
  totals: Record<string, number>
  series: { minute: number; counts: Record<string, number> }[]
}

// --- Probe and preview ---------------------------------------------------------------------

export interface FieldStat {
  path: string
  mapping_path: string
  types: Record<string, number>
  present: number
  presence: number
  distinct: number
  distinct_capped: boolean
  samples: unknown[]
  min?: number
  max?: number
}

export interface Proposal {
  target: string
  spec: unknown
  confidence: number
  reason: string
}

export interface ProbeResult {
  frames: number
  seconds: number
  link_error: string | null
  decode_errors: number
  last_decode_error: string | null
  records: number
  codec: SourceSpec['pipeline']['codec']
  suggested_records_path: string | null
  fields: FieldStat[]
  suggestion: { mapping: SourceSpec['pipeline']['mapping']; proposals: Proposal[]; identifiers: unknown[]; missing: string[] }
  samples_saved: number | null
  sample_frames: {
    received_at: string
    origin: string | null
    bytes: number
    /** Transport metadata, e.g. the MQTT `topic`; also under `_frame` in records. */
    meta?: Record<string, unknown> | null
    text: string
    truncated: boolean
  }[]
}

export interface PreviewObservation extends Observation {
  identifiers?: { scheme: string; value: string }[]
  ext?: Record<string, unknown>
  provenance?: { confidence?: number; source_code?: string }
  platform?: Record<string, string>
}

export interface PreviewResult {
  valid: boolean
  counts?: Record<string, number>
  errors?: string[]
  observations?: PreviewObservation[]
}

// --- Schema --------------------------------------------------------------------------------

export interface ExtensionField {
  key: string
  type: 'string' | 'integer' | 'number' | 'boolean' | 'enum' | 'timestamp' | 'position' | 'json'
  unit?: string
  required?: boolean
  default?: unknown
  enum_values?: string[]
  /** Notes for the people who maintain and consume the schema. */
  description?: string
  /** Filled by OpenTrack (state, speed_mps, ...) instead of a feed or card. */
  builtin?: string
}

export interface SchemaVersion {
  version: number
  status: 'draft' | 'published'
  published_at_ms: number | null
  notes: string | null
  fields: ExtensionField[]
}

export interface SchemaOverview {
  /** Always published: the OTH-GOLD minimum. */
  published_core: string[]
  /** OpenTrack values a field can be linked to, with the type the field must have. */
  builtins: { name: string; type: ExtensionField['type']; reads?: string[] }[]
  /** Which observation fields (mapping targets) fill each always-published field. */
  gold_sources?: Record<string, string[]>
  core: string[]
  reserved_extension_keys: string[]
  latest_published: number | null
  versions: SchemaVersion[]
  sources: { source: string; enabled: boolean; schema_version: number }[]
}

// --- Cards ---------------------------------------------------------------------------------

export interface RegistryIdentifier {
  scheme: string
  value: string
  expected_name?: string | null
  source?: string | null
}

export interface Entity {
  id: string
  name?: string | null
  status: string
  identifiers: RegistryIdentifier[]
}

export interface CardView {
  entity: Entity
  /** The output schema the card form follows (latest published). */
  schema: { version: number; fields: ExtensionField[] }
  card: { values: Record<string, unknown>; schema_version: number; updated_at_ms: number } | null
  revisions: {
    id: number
    values: Record<string, unknown>
    schema_version: number
    saved_at_ms: number
    decision_id: number | null
    actor: string | null
  }[]
  /** Live tracks this card speaks for, with what their feed reports. */
  tracks: {
    uid: string
    track_id: string
    state: string
    source_id: string
    last_seen: string
    feed: Record<string, unknown>
    notices: AttributeNotice[]
  }[]
}

export const api = {
  status: () => get<ServerStatus>('/status'),
  systemMetrics: (minutes = 60) => get<SystemMetrics>(`/metrics?minutes=${minutes}`),
  tracks: (limit = 10000) => get<{ total: number; tracks: TrackRow[] }>(`/tracks?limit=${limit}`),
  track: (uid: string) => get<TrackResponse>(`/tracks/${enc(uid)}`),
  explain: (uid: string) => get<{ uid: string; edges: GraphEdge[] }>(`/tracks/${enc(uid)}/explain`),

  sources: () => get<{ sources: SourceRow[] }>('/sources').then((r) => r.sources),
  source: (id: string) => get<SourceRow>(`/sources/${enc(id)}`),
  createSource: (spec: SourceSpec) => request<SourceRow>('POST', '/sources', spec),
  saveSource: (spec: SourceSpec) => request<SourceRow>('PUT', `/sources/${enc(spec.id)}`, spec),
  deleteSource: (id: string) => request<unknown>('DELETE', `/sources/${enc(id)}`),
  setEnabled: (id: string, enabled: boolean) =>
    request<SourceRow>('POST', `/sources/${enc(id)}/${enabled ? 'enable' : 'disable'}`),
  metrics: (id: string, minutes = 60) => get<MetricsResponse>(`/sources/${enc(id)}/metrics?minutes=${minutes}`),
  revisions: (id: string) =>
    get<{ revisions: { revision: number; saved_at_ms: number; decision_id: number | null; spec: SourceSpec }[] }>(
      `/sources/${enc(id)}/revisions`,
    ).then((r) => r.revisions),

  probe: (body: {
    transport: SourceSpec['transport']
    codec: SourceSpec['pipeline']['codec']
    max_frames?: number
    max_secs?: number
    save_as?: string
  }) => request<ProbeResult>('POST', '/probe', body),
  preview: (spec: SourceSpec, storedSamplesOf?: string, samples: string[] = []) =>
    request<PreviewResult>('POST', '/sources/validate', { spec, samples, stored_samples_of: storedSamplesOf }),

  schema: () => get<SchemaOverview>('/schema'),
  saveDraft: (fields: ExtensionField[], notes?: string) =>
    request<SchemaVersion>('PUT', '/schema/draft', { fields, notes }),
  publishDraft: () => request<SchemaVersion>('POST', '/schema/draft/publish'),
  discardDraft: () => request<unknown>('DELETE', '/schema/draft'),

  searchCards: (q: string) =>
    get<{ entities: (Entity & { has_card: boolean })[] }>(`/cards?q=${enc(q)}`).then((r) => r.entities),
  card: (id: string) => get<CardView>(`/cards/${enc(id)}`),
  saveCard: (id: string, values: Record<string, unknown>) =>
    request<CardView>('PUT', `/cards/${enc(id)}`, { values }),
  createCard: (body: {
    from_track?: string
    name?: string
    identifiers?: RegistryIdentifier[]
    values?: Record<string, unknown>
  }) => request<CardView>('POST', '/cards', body),
}
