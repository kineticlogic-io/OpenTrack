/**
 * The pipeline designer's live preview: the first stored samples as they are after one stage,
 * from the dry run's trace (`POST /sources/validate` with `trace`).
 */
import type { PreviewResult, TraceFrame } from '../api/client'

/** Samples the designer follows through the pipeline. */
export const TRACED_SAMPLES = 5

/** Stage names for "dropped by …", by the stage ids of `pipelineStages`. */
export const STAGE_LABEL: Record<string, string> = {
  transport: 'Transport',
  decode: 'Decode',
  reject: 'Reject',
  map: 'Map',
  join: 'Identity join',
  registry: 'Entity links',
  affiliation: 'Affiliation',
  filter: 'Filter',
  tracker: 'Tracker',
  throttle: 'Throttle',
  publish: 'Publish',
}

/** One sample (input frame) after a stage. */
export interface SampleView {
  /** 1-based, the same frame at every stage. */
  n: number
  /** Each thing the stage passed on, ready to show: pretty JSON, or the frame's text. */
  blocks: string[]
  /** What happened to the rest: dropped where, held by a stage. */
  notes: string[]
}

const pretty = (v: unknown) => JSON.stringify(v, null, 2) ?? String(v)

/** The frame as received: JSON pretty-printed, text as it is, binary escaped with its length. */
function transport(f: TraceFrame): SampleView['blocks'] {
  const { format, content } = f.frame
  return [format === 'json' ? pretty(content) : String(content)]
}

function note(stageId: string, reason: string): string {
  if (reason.startsWith('rejected:')) return reason
  return `dropped by ${STAGE_LABEL[stageId] ?? stageId}: ${reason}`
}

/**
 * The traced samples as they are after `stageId`. A frame's records are grouped under it; a
 * record dropped at or before the stage becomes a note saying where and why, and a stage that
 * held the frame back (a tracker waiting for the rest of a scan) says so.
 */
export function samplesAt(trace: TraceFrame[] | undefined, stageId: string): SampleView[] {
  return (trace ?? []).slice(0, TRACED_SAMPLES).map((f, i) => {
    const n = i + 1
    if (stageId === 'transport') {
      const notes = f.frame.format === 'binary' ? [`${f.frame.bytes} bytes, not text (shown escaped)`] : []
      return { n, blocks: transport(f), notes }
    }
    const at = f.stages.findIndex((s) => s.id === stageId)
    if (at < 0) return { n, blocks: [], notes: ['This stage was not in the pipeline the preview ran.'] }
    const notes: string[] = []
    for (const s of f.stages.slice(0, at + 1)) {
      for (const r of s.dropped ?? []) notes.push(note(s.id, r))
      if (s.held) {
        notes.push(`held by ${STAGE_LABEL[s.id] ?? s.id}: waiting for the rest of its scan (the preview ran it at the end of the samples, or when a later frame completed it)`)
      }
    }
    const blocks = f.stages[at].items.map(pretty)
    if (!blocks.length && !notes.length) notes.push('Nothing came out of this stage.')
    return { n, blocks, notes }
  })
}

/** The counts on one line, as `frames 5 · records 5 · emitted 4`. */
export function countsLine(result: PreviewResult | null): string {
  const counts = Object.entries(result?.counts ?? {}).filter(([k]) => !k.startsWith('rejected:') && !k.startsWith('grade:'))
  return counts.map(([k, v]) => `${k} ${v}`).join(' · ')
}
