/**
 * The pipeline designer's live preview: the first stored records (samples) as they are after one
 * stage, from the dry run's trace (`POST /sources/validate` with `trace`).
 */
import type { PreviewResult, PreviewTrace, TraceFrameView } from '../api/client'

/** Records the designer follows through the pipeline. */
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

/** One block of the pane: a sample after a stage, or at Transport a frame the samples came from. */
export interface SampleView {
  /** `#2`, or at Transport `frame 1 (samples #1–#5)`. */
  title: string
  /** What the stage passed on, ready to show: pretty JSON, or the frame's text. */
  blocks: string[]
  /** What happened to it: dropped where and why, held by a stage. */
  notes: string[]
}

const pretty = (v: unknown) => JSON.stringify(v, null, 2) ?? String(v)

/** `#1–#5` for a run of sample numbers, else `#1, #3`. */
function numbers(ns: number[]): string {
  const run = ns.every((n, i) => i === 0 || n === ns[i - 1] + 1)
  if (ns.length > 2 && run) return `#${ns[0]}–#${ns[ns.length - 1]}`
  return ns.map((n) => `#${n}`).join(', ')
}

/** The frame as received: JSON pretty-printed, text as it is, binary escaped with its length. */
function frameView(f: TraceFrameView | undefined, title: string): SampleView {
  if (!f) return { title, blocks: [], notes: ['The frame is not in the trace.'] }
  const notes = f.format === 'binary' ? [`${f.bytes} bytes, not text (shown escaped)`] : []
  if (f.format === 'json' && typeof f.content === 'object' && f.content !== null && 'truncated' in f.content) {
    notes.push(`${f.bytes} bytes: only the start is shown`)
  }
  return { title, blocks: [f.format === 'json' ? pretty(f.content) : String(f.content)], notes }
}

function note(stageId: string, reason: string): string {
  if (reason.startsWith('rejected:')) return reason
  return `dropped by ${STAGE_LABEL[stageId] ?? stageId}: ${reason}`
}

/**
 * The traced samples as they are after `stageId`. At Transport each frame they came from shows
 * once, naming its samples. Elsewhere each sample shows what the stage passed on; a sample
 * dropped at or before the stage says where and why, and one a stage held back (a tracker
 * waiting for the rest of a scan) says so.
 */
export function samplesAt(trace: PreviewTrace | undefined, stageId: string): SampleView[] {
  const samples = (trace?.samples ?? []).slice(0, TRACED_SAMPLES)
  if (stageId === 'transport') {
    const byFrame = new Map<number, number[]>()
    samples.forEach((s, i) => byFrame.set(s.frame, [...(byFrame.get(s.frame) ?? []), i + 1]))
    return [...byFrame].map(([frame, ns]) =>
      frameView(trace?.frames[frame], `frame ${frame + 1} (sample${ns.length > 1 ? 's' : ''} ${numbers(ns)})`),
    )
  }
  return samples.map((s, i) => {
    const title = `#${i + 1}`
    const at = s.stages.findIndex((x) => x.id === stageId)
    if (at < 0) return { title, blocks: [], notes: ['This stage was not in the pipeline the preview ran.'] }
    const notes: string[] = []
    for (const x of s.stages.slice(0, at + 1)) {
      for (const r of x.dropped ?? []) notes.push(note(x.id, r))
      if (x.held) {
        notes.push(
          `held by ${STAGE_LABEL[x.id] ?? x.id}: waiting for the rest of its scan (the preview ran it at the end of the samples, or when a later frame completed it)`,
        )
      }
    }
    const blocks = s.stages[at].items.map(pretty)
    if (!blocks.length && !notes.length) notes.push('Nothing came out of this stage.')
    return { title, blocks, notes }
  })
}

/** The counts on one line, as `frames 5 · records 5 · emitted 4`. */
export function countsLine(result: PreviewResult | null): string {
  const counts = Object.entries(result?.counts ?? {}).filter(([k]) => !k.startsWith('rejected:') && !k.startsWith('grade:'))
  return counts.map(([k, v]) => `${k} ${v}`).join(' · ')
}
