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
  peat: DependencyStatus & {
    node_id?: string
    connected_peers?: number
    sync_active?: boolean
    tracks_collection?: string
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

export interface SystemTrack {
  uid: string
  state: 'tentative' | 'confirmed' | 'lost' | 'dropped'
  view: Observation
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
  document: Record<string, unknown>
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

export class ApiError extends Error {
  readonly status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`/api/v1${path}`)
  const body = await res.json().catch(() => ({}))
  if (!res.ok) throw new ApiError(res.status, body.error ?? res.statusText)
  return body as T
}

export const api = {
  status: () => get<ServerStatus>('/status'),
  track: (uid: string) => get<TrackResponse>(`/tracks/${encodeURIComponent(uid)}`),
  explain: (uid: string) =>
    get<{ uid: string; edges: GraphEdge[] }>(`/tracks/${encodeURIComponent(uid)}/explain`),
}
