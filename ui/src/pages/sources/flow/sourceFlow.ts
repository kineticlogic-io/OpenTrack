/**
 * A source as a flow of steps (#104): the canvas is a view of the source's spec. Its steps follow the
 * pipeline's fixed order (crates/ot-source/src/pipeline.rs), so they connect themselves: adding a
 * stage puts it in its place, and removing one closes the gap. Source, Decode, Map and Output are
 * always there; the other stages are added from the palette and may be removed.
 */
import type { PluginInfo, SourceProblem, SourceSpec, SourceStage } from '../../../api/client'
import { DEFAULT_LINKS } from '../../../lib/pipeline'

type Obj = Record<string, unknown>
type Transport = SourceSpec['transport']
type Codec = SourceSpec['pipeline']['codec']

/** The steps a source can have, in the order a frame meets them. */
export const STEP_ORDER = ['source', 'decode', 'reject', 'map', 'join', 'registry', 'affiliation', 'filter', 'tracker', 'throttle', 'output'] as const
export type StepId = (typeof STEP_ORDER)[number]

/** Steps every source has: they cannot be removed. */
export const REQUIRED: ReadonlySet<StepId> = new Set(['source', 'decode', 'map', 'output'])

export type StepKind = 'source' | 'decode' | 'stage' | 'output'

export const kindOf = (step: StepId): StepKind => (step === 'source' || step === 'decode' || step === 'output' ? step : 'stage')

/** Transports, as the palette and the step heading name them, with what a new one starts as. */
export const TRANSPORTS: Record<string, { label: string; initial: Transport }> = {
  tcp_client: { label: 'TCP client', initial: { type: 'tcp_client', host: '', port: 0, framing: { type: 'lines' } } },
  tcp_server: { label: 'TCP server', initial: { type: 'tcp_server', bind: '0.0.0.0:8087', framing: { type: 'lines' } } },
  udp: { label: 'UDP', initial: { type: 'udp', bind: '0.0.0.0:6969' } },
  http_poll: { label: 'HTTP poll', initial: { type: 'http_poll', url: '', interval_secs: 5 } },
  websocket: { label: 'WebSocket', initial: { type: 'websocket', url: '' } },
  mqtt: { label: 'MQTT', initial: { type: 'mqtt', url: '', topics: [] } },
  grpc_client: { label: 'gRPC client', initial: { type: 'grpc_client', url: '', method: '' } },
  grpc_server: { label: 'gRPC server', initial: { type: 'grpc_server', bind: '0.0.0.0:50051' } },
  file: { label: 'File', initial: { type: 'file', path: '' } },
}

/** Codecs, likewise. A plugin codec is named by its plugin. */
export const CODECS: Record<string, { label: string; initial: Codec }> = {
  json: { label: 'JSON', initial: { type: 'json' } },
  cot_xml: { label: 'CoT XML', initial: { type: 'cot_xml' } },
  xml: { label: 'XML', initial: { type: 'xml', record_element: '' } },
  protobuf: { label: 'Protobuf', initial: { type: 'protobuf', files: {}, message: '' } },
}

/** The stages added from the palette, with what a new one starts as. */
const STAGES: Record<Exclude<StepId, 'source' | 'decode' | 'map' | 'output'>, { label: string; add: (s: SourceSpec) => SourceSpec; remove: (s: SourceSpec) => SourceSpec }> = {
  reject: {
    label: 'Reject',
    add: (s) => withPipeline(s, { mapping: { ...s.pipeline.mapping, reject: [{ reason: 'no_position', when: { path: 'lat', exists: false } }] } }),
    remove: (s) => withPipeline(s, { mapping: { ...s.pipeline.mapping, reject: undefined } }),
  },
  join: {
    label: 'Static join',
    add: (s) => withPipeline(s, { static_join: {} }),
    remove: (s) => withPipeline(s, { static_join: undefined }),
  },
  registry: {
    label: 'Entity links',
    add: (s) => withPipeline(s, { registry: { links: DEFAULT_LINKS } }),
    remove: (s) => withPipeline(s, { registry: undefined }),
  },
  affiliation: {
    label: 'Affiliation',
    add: (s) => withPipeline(s, { affiliation: { country: 'platform.flag', friendly: [], hostile: [], neutral: [] } }),
    remove: (s) => withPipeline(s, { affiliation: undefined }),
  },
  filter: {
    label: 'Filter',
    add: (s) => withPipeline(s, { filter: {} }),
    remove: (s) => withPipeline(s, { filter: undefined }),
  },
  tracker: {
    label: 'Tracker',
    add: (s) => withPipeline(s, { tracker: { algorithm: 'gnn' } }),
    remove: (s) => withPipeline(s, { tracker: undefined }),
  },
  throttle: {
    label: 'Throttle',
    add: (s) => withPipeline(s, { throttle: { min_interval_secs: 5, min_move_m: 0, heartbeat_secs: 600 } }),
    remove: (s) => withPipeline(s, { throttle: undefined }),
  },
}

const OUTPUTS: Record<'tracks' | 'detections', string> = { tracks: 'Tracks', detections: 'Detections' }

/** Set pipeline keys; one set to undefined is dropped, so the spec carries only what is configured. */
function withPipeline(spec: SourceSpec, patch: Obj): SourceSpec {
  const pipeline: Obj = { ...spec.pipeline, ...patch }
  for (const [k, v] of Object.entries(patch)) if (v === undefined) delete pipeline[k]
  const mapping = pipeline.mapping as Obj
  if (mapping && mapping.reject === undefined) delete mapping.reject
  return { ...spec, pipeline: pipeline as SourceSpec['pipeline'] }
}

const rejects = (spec: SourceSpec) => ((spec.pipeline.mapping.reject as unknown[] | undefined) ?? []).length > 0

/** The steps this source has, in order. */
export function stepsOf(spec: SourceSpec): StepId[] {
  const p = spec.pipeline as Obj
  const has: Record<StepId, boolean> = {
    source: true,
    decode: true,
    reject: rejects(spec),
    map: true,
    join: p.static_join !== undefined,
    registry: p.registry !== undefined,
    affiliation: p.affiliation !== undefined,
    filter: p.filter !== undefined,
    tracker: p.tracker !== undefined,
    throttle: p.throttle !== undefined,
    output: true,
  }
  return STEP_ORDER.filter((s) => has[s])
}

/** A step's heading: what it is for this source (its transport, codec, or output). */
export function stepTitle(spec: SourceSpec, step: StepId): string {
  switch (step) {
    case 'source':
      return TRANSPORTS[spec.transport.type]?.label ?? spec.transport.type
    case 'decode': {
      const c = spec.pipeline.codec
      if (c.type === 'plugin') return String(c.plugin || 'Plugin')
      return CODECS[c.type]?.label ?? c.type
    }
    case 'map':
      return 'Map'
    case 'output':
      return OUTPUTS[spec.reports ?? 'tracks']
    default:
      return STAGES[step].label
  }
}

/** The word under a step's heading. */
export const KIND_WORD: Record<StepKind, string> = { source: 'Source', decode: 'Decode', stage: 'Stage', output: 'Output' }

/** The trace's name for a step (crates/ot-source/src/trace.rs; `samplesAt`). */
export const traceStage = (step: StepId): string => (step === 'source' ? 'transport' : step === 'output' ? 'publish' : step)

/** The step a problem belongs to, from the server's pipeline stage; `null`: the source as a whole. */
const STEP_OF_STAGE: Record<SourceStage, StepId> = {
  source: 'source',
  decode: 'decode',
  map: 'map',
  static_join: 'join',
  registry: 'registry',
  affiliation: 'affiliation',
  filter: 'filter',
  tracker: 'tracker',
  throttle: 'throttle',
  output: 'output',
}

export interface StepProblems {
  byStep: Partial<Record<StepId, string[]>>
  /** Problems with the source as a whole, or with a stage it does not show. */
  general: string[]
}

/** The server's problems, each on its step. */
export function problemsByStep(problems: SourceProblem[], steps: StepId[]): StepProblems {
  const out: StepProblems = { byStep: {}, general: [] }
  for (const p of problems) {
    const step = p.stage ? STEP_OF_STAGE[p.stage] : null
    if (step && steps.includes(step)) (out.byStep[step] ??= []).push(p.message)
    else out.general.push(p.message)
  }
  return out
}

/** A palette item's type: `source:<transport>`, `decode:<codec>` (`decode:plugin:<name>`), `stage:<step>` or `output:<reports>`. */
export interface PaletteEntry {
  type: string
  label: string
  group: 'Source' | 'Decode' | 'Stages' | 'Output'
}

export function paletteEntries(plugins: PluginInfo[]): PaletteEntry[] {
  return [
    ...Object.entries(TRANSPORTS).map(([t, d]) => ({ type: `source:${t}`, label: d.label, group: 'Source' as const })),
    ...Object.entries(CODECS).map(([c, d]) => ({ type: `decode:${c}`, label: d.label, group: 'Decode' as const })),
    ...plugins.filter((p) => p.kinds.includes('codec') && p.enabled).map((p) => ({ type: `decode:plugin:${p.name}`, label: p.name, group: 'Decode' as const })),
    ...(Object.keys(STAGES) as (keyof typeof STAGES)[]).map((s) => ({ type: `stage:${s}`, label: STAGES[s].label, group: 'Stages' as const })),
    ...(Object.keys(OUTPUTS) as (keyof typeof OUTPUTS)[]).map((r) => ({ type: `output:${r}`, label: OUTPUTS[r], group: 'Output' as const })),
  ]
}

/** The step a palette item makes or replaces. */
export function stepOfEntry(type: string): StepId | null {
  const [kind, sub] = type.split(':')
  if (kind === 'source' || kind === 'decode' || kind === 'output') return kind
  if (kind === 'stage' && sub in STAGES) return sub as StepId
  return null
}

const streamed = (t: Transport) => t.type === 'tcp_client' || t.type === 'tcp_server'

const LINES = { type: 'lines' }
const COT_FRAMING = { type: 'end_tag', tag: '</event>' }
const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b)

/** CoT arrives as back-to-back XML events, framed by `</event>` rather than by lines on a stream. */
function cotFraming(t: Transport, codec: Codec): Transport {
  const framing = t.framing as { type?: string } | undefined
  return codec.type === 'cot_xml' && streamed(t) && framing?.type === 'lines' ? { ...t, framing: COT_FRAMING } : t
}

/** The framing a codec sets on a stream when chosen (CoT's end tag, a plugin's own), if any. */
function framingOf(codec: Codec, plugins: PluginInfo[]): unknown {
  if (codec.type === 'cot_xml') return COT_FRAMING
  if (codec.type === 'plugin') return plugins.find((p) => p.name === codec.plugin)?.framing ?? null
  return null
}

/**
 * Whether adding this palette item would replace settings the source has: a different transport,
 * codec or output in place of the one it has. A stage it already has is not added again.
 */
export function replaces(spec: SourceSpec, type: string): boolean {
  const [kind, sub, name] = type.split(':')
  if (kind === 'source') return sub !== spec.transport.type
  if (kind === 'decode') {
    const c = spec.pipeline.codec
    return sub === 'plugin' ? c.type !== 'plugin' || c.plugin !== name : sub !== c.type
  }
  return false
}

/** The source with this palette item added: a stage put in its place, or a transport, codec or output swapped in. */
export function addEntry(spec: SourceSpec, type: string, plugins: PluginInfo[]): SourceSpec {
  const [kind, sub, name] = type.split(':')
  switch (kind) {
    case 'source': {
      const d = TRANSPORTS[sub]
      if (!d || sub === spec.transport.type) return spec
      const next: SourceSpec = { ...spec, transport: cotFraming(structuredClone(d.initial), spec.pipeline.codec) }
      // The acceptance belongs to the listener it was given for.
      delete next.unauthenticated
      return next
    }
    case 'decode': {
      // The framing the old codec set goes with it.
      const before = framingOf(spec.pipeline.codec, plugins)
      if (before && streamed(spec.transport) && same(spec.transport.framing, before)) {
        spec = { ...spec, transport: { ...spec.transport, framing: LINES } }
      }
      let codec: Codec | null = null
      if (sub === 'plugin') {
        const p = plugins.find((x) => x.name === name)
        if (p && !(spec.pipeline.codec.type === 'plugin' && spec.pipeline.codec.plugin === p.name)) {
          codec = { type: 'plugin', plugin: p.name, options: structuredClone(p.default_options) }
          // A stream of the plugin's format is framed its way.
          if (p.framing && streamed(spec.transport)) spec = { ...spec, transport: { ...spec.transport, framing: structuredClone(p.framing) } }
        }
      } else if (CODECS[sub] && sub !== spec.pipeline.codec.type) {
        codec = structuredClone(CODECS[sub].initial)
      }
      if (!codec) return spec
      return { ...withPipeline(spec, { codec }), transport: cotFraming(spec.transport, codec) }
    }
    case 'stage': {
      const s = STAGES[sub as keyof typeof STAGES]
      if (!s || stepsOf(spec).includes(sub as StepId)) return spec
      return s.add(spec)
    }
    case 'output': {
      if (sub !== 'tracks' && sub !== 'detections') return spec
      const next: SourceSpec = { ...spec, reports: sub }
      if (sub === 'tracks') delete next.reports
      return next
    }
  }
  return spec
}

/** The source without this stage. Source, Decode, Map and Output stay. */
export function removeStep(spec: SourceSpec, step: StepId): SourceSpec {
  if (REQUIRED.has(step)) return spec
  return STAGES[step as keyof typeof STAGES].remove(spec)
}

/** Where each step sits: one column, top to bottom. */
export const STEP_GAP = 96
export const positionOf = (index: number) => ({ x: 0, y: index * STEP_GAP })

/** A new source: its transport, an empty JSON decode and mapping, reporting tracks. */
export function newSourceSpec(id: string, name: string, transport: string): SourceSpec {
  return {
    id,
    name,
    transport: structuredClone(TRANSPORTS[transport]?.initial ?? TRANSPORTS.http_poll.initial),
    pipeline: { codec: { type: 'json' }, mapping: { rules: [] } },
  }
}

/** A source id from its name: lowercase letters, digits, `-` and `_`, at most 64 (crates/ot-source/src/source.rs). */
export function idFromName(name: string): string {
  return name
    .toLowerCase()
    .normalize('NFKD')
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
}

export const validId = (id: string) => /^[a-z0-9_-]{1,64}$/.test(id)
