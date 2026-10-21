import { describe, expect, it } from 'vitest'
import type { PluginInfo, SourceSpec } from '../../../api/client'
import {
  addEntry,
  idFromName,
  newSourceSpec,
  paletteEntries,
  problemsByStep,
  REQUIRED,
  removeStep,
  replaces,
  STEP_ORDER,
  stepOfEntry,
  stepsOf,
  stepTitle,
  validId,
} from './sourceFlow'

/** Every source spec the docs ship (docs/examples). */
const files = import.meta.glob<SourceSpec>('../../../../../docs/examples/**/*.json', { import: 'default', eager: true })
const examples = Object.entries(files).filter(([, v]) => typeof v === 'object' && v !== null && 'transport' in v && 'pipeline' in v)

const plugin = (name: string, framing: Record<string, unknown> | null = null): PluginInfo =>
  ({ name, kinds: ['codec'], enabled: true, default_options: { a: 1 }, framing }) as unknown as PluginInfo

describe('stepsOf', () => {
  it('finds examples to check', () => {
    expect(examples.length).toBeGreaterThan(5)
  })

  it.each(examples)('%s: every required step, in pipeline order', (_, spec) => {
    const steps = stepsOf(spec)
    for (const r of REQUIRED) expect(steps).toContain(r)
    expect(steps).toEqual(STEP_ORDER.filter((s) => steps.includes(s)))
  })

  it.each(examples)('%s: each added stage removes back to the source as it was', (_, spec) => {
    for (const entry of paletteEntries([]).filter((e) => e.group === 'Stages')) {
      const step = stepOfEntry(entry.type)!
      const had = stepsOf(spec).includes(step)
      const added = addEntry(spec, entry.type, [])
      expect(stepsOf(added)).toContain(step)
      if (had) expect(added).toBe(spec)
      else expect(removeStep(added, step)).toEqual(spec)
    }
  })
})

describe('a new source', () => {
  it('has its Source, Decode, Map and Output steps', () => {
    const s = newSourceSpec('ais', 'AIS', 'websocket')
    expect(stepsOf(s)).toEqual(['source', 'decode', 'map', 'output'])
    expect(s.transport).toEqual({ type: 'websocket', url: '' })
    expect(stepTitle(s, 'source')).toBe('WebSocket')
    expect(stepTitle(s, 'decode')).toBe('JSON')
    expect(stepTitle(s, 'output')).toBe('Tracks')
  })

  it('takes its id from its name', () => {
    expect(idFromName('adsb.lol feed (EU)')).toBe('adsb-lol-feed-eu')
    expect(validId(idFromName('adsb.lol feed (EU)'))).toBe(true)
    expect(validId('Bad Id')).toBe(false)
    expect(validId('')).toBe(false)
  })
})

describe('palette entries', () => {
  const base = newSourceSpec('x', 'X', 'tcp_client')

  it('swap the transport, and the acceptance that belonged to the old one goes', () => {
    const udp = addEntry(base, 'source:udp', [])
    expect(udp.transport.type).toBe('udp')
    const accepted: SourceSpec = { ...udp, unauthenticated: 'accepted' }
    expect(replaces(accepted, 'source:mqtt')).toBe(true)
    expect(replaces(accepted, 'source:udp')).toBe(false)
    const mqtt = addEntry(accepted, 'source:mqtt', [])
    expect(mqtt.transport).toEqual({ type: 'mqtt', url: '', topics: [] })
    expect(mqtt.unauthenticated).toBeUndefined()
    expect(addEntry(accepted, 'source:udp', [])).toBe(accepted)
  })

  it('frame CoT on a stream by its end tag', () => {
    const cot = addEntry(base, 'decode:cot_xml', [])
    expect(cot.pipeline.codec).toEqual({ type: 'cot_xml' })
    expect(cot.transport.framing).toEqual({ type: 'end_tag', tag: '</event>' })
    const server = addEntry(cot, 'source:tcp_server', [])
    expect(server.transport.framing).toEqual({ type: 'end_tag', tag: '</event>' })
  })

  it('take a codec plugin with its options and framing', () => {
    const p = plugin('stanag4607', { type: 'length_field', offset: 2, width: 'u32' })
    expect(paletteEntries([p]).map((e) => e.type)).toContain('decode:plugin:stanag4607')
    const s = addEntry(base, 'decode:plugin:stanag4607', [p])
    expect(s.pipeline.codec).toEqual({ type: 'plugin', plugin: 'stanag4607', options: { a: 1 } })
    expect(s.transport.framing).toEqual({ type: 'length_field', offset: 2, width: 'u32' })
    expect(stepTitle(s, 'decode')).toBe('stanag4607')
    expect(replaces(s, 'decode:plugin:stanag4607')).toBe(false)
    expect(replaces(s, 'decode:json')).toBe(true)
  })

  it('set what the output reports', () => {
    const plots = addEntry(base, 'output:detections', [])
    expect(plots.reports).toBe('detections')
    expect(stepTitle(plots, 'output')).toBe('Detections')
    expect('reports' in addEntry(plots, 'output:tracks', [])).toBe(false)
  })

  it('never remove a required step', () => {
    for (const r of REQUIRED) expect(removeStep(base, r)).toBe(base)
  })
})

describe('problemsByStep', () => {
  it('puts each problem on its step, the rest with the source', () => {
    const steps = stepsOf(newSourceSpec('x', 'X', 'tcp_client'))
    const p = problemsByStep(
      [
        { kind: 'incomplete', stage: 'source', field: 'host', message: 'the host is not set' },
        { kind: 'incomplete', stage: 'map', message: 'mapping has no rules' },
        { kind: 'incomplete', stage: 'static_join', message: 'not shown' },
        { kind: 'invalid', stage: null, field: 'name', message: 'source name must not be empty' },
      ],
      steps,
    )
    expect(p.byStep).toEqual({ source: ['the host is not set'], map: ['mapping has no rules'] })
    expect(p.general).toEqual(['not shown', 'source name must not be empty'])
  })
})
