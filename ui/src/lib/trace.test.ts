import { describe, expect, it } from 'vitest'
import type { TraceFrame } from '../api/client'
import { countsLine, samplesAt } from './trace'

const trace: TraceFrame[] = [
  {
    frame: { format: 'json', bytes: 20, content: { id: 'A', lat: 1 } },
    stages: [
      { id: 'decode', items: [{ id: 'A', lat: 1 }] },
      { id: 'map', items: [{ source_track_key: 'A' }] },
      { id: 'filter', items: [{ source_track_key: 'A' }] },
      { id: 'publish', items: [{ op: 'upsert', name: 'A' }] },
    ],
  },
  {
    frame: { format: 'json', bytes: 40, content: { plots: [1, 2, 3] } },
    stages: [
      { id: 'decode', items: [{ n: 1 }, { n: 2 }, { n: 3 }] },
      { id: 'reject', items: [{ n: 1 }, { n: 3 }], dropped: ['rejected: no_position'] },
      { id: 'map', items: [{ source_track_key: '1' }, { source_track_key: '3' }] },
      { id: 'filter', items: [{ source_track_key: '1' }], dropped: ['drop_if matched'] },
      { id: 'tracker', items: [], held: true },
      { id: 'publish', items: [] },
    ],
  },
  {
    frame: { format: 'text', bytes: 8, content: 'not json' },
    stages: [
      { id: 'decode', items: [], dropped: ['decode error: bad'] },
      { id: 'map', items: [] },
      { id: 'filter', items: [] },
      { id: 'publish', items: [] },
    ],
  },
  { frame: { format: 'binary', bytes: 2, content: '\\x01\\xff' }, stages: [] },
]

describe('samplesAt', () => {
  it('shows the frames as received at Transport', () => {
    const s = samplesAt(trace, 'transport')
    expect(s.map((x) => x.n)).toEqual([1, 2, 3, 4])
    expect(s[0].blocks).toEqual(['{\n  "id": "A",\n  "lat": 1\n}'])
    expect(s[2].blocks).toEqual(['not json'])
    expect(s[3].blocks).toEqual(['\\x01\\xff'])
    expect(s[3].notes[0]).toMatch(/^2 bytes/)
  })

  it('follows each sample to the selected stage, with what was dropped on the way', () => {
    const map = samplesAt(trace, 'map')
    expect(map[0].blocks).toEqual([JSON.stringify({ source_track_key: 'A' }, null, 2)])
    expect(map[1].blocks).toHaveLength(2)
    expect(map[1].notes).toEqual(['rejected: no_position'])
    expect(map[2].notes).toEqual(['dropped by Decode: decode error: bad'])

    const filter = samplesAt(trace, 'filter')
    expect(filter[1].blocks).toHaveLength(1)
    expect(filter[1].notes).toEqual(['rejected: no_position', 'dropped by Filter: drop_if matched'])
    expect(filter[2].notes).toEqual(['dropped by Decode: decode error: bad'])
    // Not a stage of this frame's (older) trace.
    expect(filter[3].notes[0]).toMatch(/not in the pipeline/)
  })

  it('says when a stage holds a sample, and when nothing came out', () => {
    const tracker = samplesAt(trace, 'tracker')
    expect(tracker[1].blocks).toEqual([])
    expect(tracker[1].notes.at(-1)).toMatch(/^held by Tracker/)
    const publish = samplesAt(trace, 'publish')
    expect(publish[0].blocks[0]).toContain('"op": "upsert"')
    expect(publish[1].notes.some((n) => n.startsWith('held by Tracker'))).toBe(true)
    expect(samplesAt([{ frame: trace[0].frame, stages: [{ id: 'map', items: [] }] }], 'map')[0].notes).toEqual(['Nothing came out of this stage.'])
  })

  it('shows at most five samples, and none without a trace', () => {
    expect(samplesAt([...trace, ...trace], 'transport')).toHaveLength(5)
    expect(samplesAt(undefined, 'map')).toEqual([])
  })
})

describe('countsLine', () => {
  it('puts the counts on one line without the per-reason ones', () => {
    expect(countsLine({ valid: true, counts: { frames: 5, records: 7, 'rejected:no_position': 1, 'grade:none': 4, emitted: 4 } })).toBe(
      'frames 5 · records 7 · emitted 4',
    )
    expect(countsLine(null)).toBe('')
  })
})
