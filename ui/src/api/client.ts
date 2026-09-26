// Typed client for the OpenTrack control plane (`/api/v1`).

export interface DependencyStatus {
  ok: boolean
  error?: string
  [key: string]: unknown
}

export interface ServerStatus {
  service: string
  version: string
  /** Versions of the correlation engine and trackers (see docs/algorithms.md). */
  algorithms?: { correlation: string; trackers: Record<string, string> }
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

/** A track field where the entity's value replaced a different one the feed reported; the entity's is published. */
export interface AttributeNotice {
  /** Track field, e.g. classification.cot_type or ext.destination. */
  key: string
  entity: unknown
  feed: unknown
  source_id: string
}

export interface SystemTrack {
  uid: string
  state: 'tentative' | 'confirmed' | 'lost' | 'dropped'
  view: Observation
  /** The registry entity this track resolves to. */
  entity_id?: string
  kind?: 'track' | 'group'
  /** A group's members (UIDs). */
  members?: string[]
  /** Groups it belongs to (`tms-<UID>`). */
  groups?: string[]
  /** Tracks paired with it (UIDs). */
  paired_with?: string[]
  /** Published attributes, resolved from the track's values (feeds and entity links) and built-ins. */
  attributes?: Record<string, unknown>
  notices?: AttributeNotice[]
  contributors: {
    source_id: string
    source_track_key: string
    pairing: 'auto' | 'manual'
    /** Probability that it reports the same object as the rest of the track. */
    confidence: number
    last_report: string
    /** The source's own probability that the object exists (a tracker's). */
    existence?: number
  }[]
  first_seen: string
  last_seen: string
  observation_count: number
  /** False while the track is kept inside OpenTrack (not yet authoritative). */
  published?: boolean
  /** Why the output filter holds it back, when it does. */
  filtered?: string | null
}

export interface TrackResponse {
  /** Probability that the track is a real object (its sources' existence and pairing confidences). */
  confidence?: number
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
  /** The source node's attributes, e.g. `tracker` for a source track a tracker formed. */
  src_attrs?: Record<string, unknown>
  valid_from_ms: number
  valid_to_ms: number | null
  decision_id: number
  decision_op: string
  decision_actor: string
  decision_reason?: string | null
  ended_by: number | null
}

// --- Correlation ---------------------------------------------------------------------------

/** Correlation settings as the server stores them (see correlate::CorrelationSettings). */
export interface CorrelationSettings {
  approach: 'identifiers' | 'kinematics' | 'kinematics_metadata'
  mode: 'automatic' | 'suggest'
  kinematic: {
    gate_probability: number
    min_sigma_m: number
    process_noise_mps2: number
    speed_sigma_mps: number
    object_density_per_km2: number
    velocity_spread_mps: number
    prior_probability: number
    pair_probability: number
    m: number
    n: number
    window_secs: number
    min_interval_secs: number
    max_age_secs: number
  }
  gate: { base_m: number; max_extrapolation_secs: number }
  freshness_secs: number
  split: { propose: boolean; automatic: boolean; split_probability: number; gate_probability: number; m: number; n: number }
  output: OutputFilter
}

export interface CorrelationSettingsResponse {
  settings: CorrelationSettings
  saved: boolean
  defaults: CorrelationSettings
  version: string
}

/** A live track as a suggestion shows it. */
export interface SuggestionTrack {
  uid: string
  track_id: string
  live: boolean
  name?: string | null
  state?: string
  published?: boolean
  latitude?: number
  longitude?: number
  sources?: string[]
}

export interface Suggestion {
  id: number
  kind: 'pair' | 'split'
  track_a: string
  track_b?: string
  source_track?: string
  evidence: Record<string, unknown> & { reason?: string }
  status: 'open' | 'accepted' | 'rejected' | 'expired'
  created_at_ms: number
  updated_at_ms: number
  decision_id?: number
  a: SuggestionTrack
  b: SuggestionTrack | null
}

export interface DecisionRow {
  id: number
  at_ms: number
  actor: string
  op: string
  reason: string | null
  evidence: Record<string, unknown> | null
}

/** A row of the live tracks list: GOLD fields as published plus what the table shows. */
export interface TrackRow {
  uid: string
  track_id: string
  /** `group` for a group a track manager formed; absent for a track. */
  kind?: 'track' | 'group'
  /** A group's members (`tms-<UID>`). */
  members?: string[]
  /** Groups this track belongs to (`tms-<UID>`). */
  groups?: string[]
  /** Tracks paired with this one (GOLD PAIR), `tms-<UID>`. */
  paired_with?: string[]
  entity_id: string | null
  /** Track fields where the entity replaced what a feed reports. */
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
  published?: boolean
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
    with_entity: number
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

/** OpenStare's reserved `stare-security` fields; free text until vocabularies are defined. */
export interface SecurityLabel {
  classification: string
  restrictions?: string[]
  sharing?: string
}

/** What OpenTrack publishes (correlation settings `output`). */
export interface OutputFilter {
  areas: { name: string; exclude: boolean; min_lat: number; min_lon: number; max_lat: number; max_lon: number }[]
  affiliations: string[]
  domains: string[]
  track_types: string[]
  min_confidence: number
  rule?: unknown
}

/** The classification banner as OpenStare shapes it. */
export interface Banner {
  enabled: boolean
  text: string
  background: string
  color: string
}

export interface AppSettings {
  site_name: string
  banner: Banner
}

export interface AppSettingsResponse {
  settings: AppSettings
  site_code: string
  node_id: string
}

/** What a spreadsheet import does, row by row. */
export interface SheetImport {
  applied: boolean
  counts: { create: number; update: number; unchanged: number; error: number }
  rows: {
    row: number
    action: 'create' | 'update' | 'unchanged' | 'error'
    entity_id: string
    name?: string
    identifiers_added?: string[]
    /** Fields the row changes (the minimum's and attributes). */
    fields?: string[]
    errors?: string[]
  }[]
}

/** A codec plugin and the options it takes. */
export interface CodecPlugin {
  name: string
  version: string
  description: string
  options: (
    | { name: string; label: string; help: string; type: 'bool'; default: boolean }
    | { name: string; label: string; help: string; type: 'choice'; choices: string[]; default: string }
    | { name: string; label: string; help: string; type: 'number'; default: number | null; unit: string; min: number | null }
  )[]
  default_options: Record<string, unknown>
  framing: Record<string, unknown> | null
}
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
  /** Tracks (a key per object) or detections (anonymous plots). */
  reports?: 'tracks' | 'detections'
  /** Whether a track this source alone reports for is published (default: track feeds only). */
  publish_alone?: boolean
  /** Security label for everything the source reports (OpenStare's `stare-security` shape). */
  security?: SecurityLabel
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
  /** The windows a tracker with auto timing chose from the sensor's revisit rate. */
  tracker_timing?: {
    revisit_secs: number
    revisit_source: string
    confirm_within_secs: number
    drop_tentative_secs: number
    drop_confirmed_secs: number
  } | null
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
  /** Filled by OpenTrack (state, speed_mps, ...) instead of a feed or entity. */
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

// --- Track management ----------------------------------------------------------------------

/** What a group is: name and symbol, plus the symbol's parts the editor keeps. */
export interface GroupSpec {
  name: string
  sidc: string
  affiliation?: string | null
  domain?: string | null
  description?: string | null
  /** Group class published as the GOLD class-name, e.g. CARRIER STRIKE GROUP. */
  class?: string | null
  /** How the symbol was built (the editor's choices). */
  base?: string | null
  echelon?: string | null
  task_force?: boolean
  sidc_override?: boolean
}

export interface TrackGroup {
  track_id: string
  uid: string
  spec: GroupSpec
  members: string[]
  created_at_ms: number
  updated_at_ms: number
}

// --- Entities --------------------------------------------------------------------------

export interface RegistryIdentifier {
  scheme: string
  value: string
  /** Name the object is expected to broadcast under this identifier. */
  expected_name?: string | null
  source?: string | null
}

export type AttrType = 'text' | 'number' | 'boolean' | 'datetime' | 'json'
export const ATTR_TYPES: AttrType[] = ['text', 'number', 'boolean', 'datetime', 'json']

export interface EntityAttribute {
  key: string
  type: AttrType
  value: unknown
}

export const DOMAINS = ['air', 'surface', 'subsurface', 'ground', 'space'] as const
export const AFFILIATIONS = ['pending', 'unknown', 'assumed_friend', 'friend', 'neutral', 'suspect', 'hostile', 'joker', 'faker', 'none'] as const
export const TRACK_TYPES = ['tactical', 'live_training', 'simulated_training', 'demand_entry'] as const

/** One real-world object: identifiers, status, the OTH-GOLD minimum and free-form attributes. */
export interface Entity {
  id: string
  status: 'active' | 'retired'
  name?: string | null
  class_name?: string | null
  domain?: (typeof DOMAINS)[number] | null
  affiliation?: (typeof AFFILIATIONS)[number] | null
  track_type?: (typeof TRACK_TYPES)[number] | null
  cot_type?: string | null
  sidc?: string | null
  identifiers: RegistryIdentifier[]
  attributes: EntityAttribute[]
  source?: string | null
  updated_at_ms?: number
}

export interface EntityView {
  entity: Entity
  /** Saved versions, newest first. */
  revisions: { id: number; entity: Entity; saved_at_ms: number; decision_id: number | null; actor: string | null }[]
  /** Live tracks resolving to it, with the fields where it replaced what their feed reports. */
  tracks: { uid: string; track_id: string; state: string; source_id: string; last_seen: string; notices: AttributeNotice[] }[]
}

export const api = {
  status: () => get<ServerStatus>('/status'),
  systemMetrics: (minutes = 60) => get<SystemMetrics>(`/metrics?minutes=${minutes}`),
  tracks: (limit = 10000) => get<{ total: number; tracks: TrackRow[] }>(`/tracks?limit=${limit}`),
  track: (uid: string) => get<TrackResponse>(`/tracks/${enc(uid)}`),
  explain: (uid: string) => get<{ uid: string; edges: GraphEdge[] }>(`/tracks/${enc(uid)}/explain`),
  correlationSettings: () => get<CorrelationSettingsResponse>('/correlation/settings'),
  saveCorrelationSettings: (s: CorrelationSettings) => request<{ settings: CorrelationSettings }>('PUT', '/correlation/settings', s),
  suggestions: (status = 'open') => get<{ suggestions: Suggestion[] }>(`/correlation/suggestions?status=${enc(status)}`),
  decideSuggestion: (id: number, decision: 'accept' | 'reject') =>
    request<Record<string, unknown>>('POST', `/correlation/suggestions/${id}/${decision}`),
  correlationDecisions: (limit = 100) => get<{ decisions: DecisionRow[] }>(`/correlation/decisions?limit=${limit}`),
  splitTrack: (uid: string, sourceTrack: string) =>
    request<{ new_track: string }>('POST', `/tracks/${enc(uid)}/split`, { source_track: sourceTrack }),
  /** Merge `from` into `into`; with `hold` (a track manager's merge, GOLD MRG) correlation never splits it. */
  mergeTracks: (from: string, into: string, hold = false) => request<{ merged_into: string }>('POST', '/tracks/merge', { from, into, hold }),
  pairTracks: (tracks: string[]) => request<{ paired: string[] }>('POST', '/tracks/pair', { tracks }),
  unpairTracks: (a: string, b: string) => request<unknown>('POST', '/tracks/unpair', { a, b }),
  deleteTracks: (tracks: string[]) => request<{ deleted: string[] }>('POST', '/tracks/delete', { tracks }),
  groups: () => get<{ groups: TrackGroup[] }>('/groups').then((r) => r.groups),
  createGroup: (spec: GroupSpec, members: string[]) => request<{ group: string }>('POST', '/groups', { spec, members }),
  updateGroup: (id: string, spec: GroupSpec) => request<unknown>('PUT', `/groups/${enc(id)}`, { spec }),
  groupMembers: (id: string, add: string[], remove: string[]) => request<unknown>('POST', `/groups/${enc(id)}/members`, { add, remove }),
  dissolveGroup: (id: string) => request<unknown>('DELETE', `/groups/${enc(id)}`),
  doNotPair: (a: string, b: string) => request<Record<string, unknown>>('POST', '/tracks/do-not-pair', { a, b }),

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

  plugins: () => get<{ plugins: CodecPlugin[] }>('/plugins').then((r) => r.plugins),

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

  appSettings: () => get<AppSettingsResponse>('/settings'),
  saveAppSettings: (settings: AppSettings) => request<AppSettingsResponse>('PUT', '/settings', settings),
  banner: () => get<Banner>('/public/banner'),
  exportUrl: (what: 'tracks.geojson' | 'tracks.csv' | 'config') =>
    what === 'config' ? '/api/v1/export/config' : `/api/v1/export/tracks?format=${what === 'tracks.csv' ? 'csv' : 'geojson'}`,
  purge: (confirm: string, history: boolean) =>
    request<{ retired: number; history?: { nodes: number; edges: number } | null }>('POST', '/admin/purge', { confirm, history }),

  registryEntities: (q: string, limit = 100, offset = 0) =>
    get<{ entities: Entity[]; total: number }>(`/registry/entities?q=${enc(q)}&limit=${limit}&offset=${offset}`),
  entity: (id: string) => get<EntityView>(`/registry/entities/${enc(id)}`),
  /** Create an entity; with `from_track`, seeded with that live track's name and identifiers. */
  createEntity: (entity: Omit<Entity, 'id'> & { from_track?: string }) => request<EntityView>('POST', '/registry/entities', entity),
  saveEntity: (entity: Entity) => request<EntityView>('PUT', `/registry/entities/${enc(entity.id)}`, entity),
  deleteEntity: (id: string) => request<unknown>('DELETE', `/registry/entities/${enc(id)}`),
  /** Entity fields a pipeline can link: the minimum and the attribute keys in use. */
  registryFields: () => get<{ minimum: string[]; attributes: { key: string; entities: number }[] }>('/registry/fields'),
  registryExportUrl: (format: 'xlsx' | 'csv') => `/api/v1/registry/export?format=${format}`,
  /** Plan (or with `apply`, make) the changes a spreadsheet describes. */
  importSheet: async (file: File, apply: boolean): Promise<SheetImport> => {
    const format = file.name.toLowerCase().endsWith('.csv') ? 'csv' : 'xlsx'
    const res = await fetch(`/api/v1/registry/import-sheet?format=${format}&apply=${apply}&label=${enc(file.name)}`, {
      method: 'POST',
      headers: { 'x-opentrack-actor': ACTOR },
      body: file,
    })
    const parsed = JSON.parse((await res.text()) || '{}')
    if (!res.ok && !parsed.rows) throw new ApiError(res.status, parsed.error ?? res.statusText)
    return parsed as SheetImport
  },

}
