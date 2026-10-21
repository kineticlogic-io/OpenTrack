import { TbDownload, TbWand } from 'react-icons/tb'
import { Button, Input, Label } from '@kineticlogic/staresdk'
import type { PreviewResult, ProbeResult, SchemaOverview, SourceSpec } from '../../../api/client'
import { InfoTip } from '../../../components/InfoTip'
import { sampleFields } from '../../../lib/conditions'
import { filterInput } from '../../../lib/trace'
import { MapEditor } from '../designer/MapEditor'
import { StageSamples } from '../designer/StageSamples'
import {
  AffiliationForm,
  EmitterMotionForm,
  FilterForm,
  JoinForm,
  PublishForm,
  RegistryForm,
  RejectForm,
  SecurityForm,
  ThrottleForm,
  TrackerForm,
} from '../designer/StageForms'
import { SenderAuth } from '../SenderAuth'
import { TransportForm } from '../TransportForm'
import { KIND_WORD, kindOf, stepTitle, traceStage, type StepId } from './sourceFlow'

type Obj = Record<string, unknown>
type Pipeline = SourceSpec['pipeline'] & Obj

/** What each step does, for the ⓘ beside its heading. */
const STEP_HELP: Record<StepId, string> = {
  source: 'Where frames come from: the transport, how it connects or listens, and how a stream is cut into frames.',
  decode: 'Turns each frame into records.',
  reject: 'Drops raw records before mapping, each rule with a reason counted in the source’s metrics.',
  map: 'Turns records into observations: the track key, identifiers and OpenTrack fields each rule takes from the record.',
  join: 'Keeps the identity fields that static rules report per track key, and fills them into later observations with the same key.',
  registry: 'Finds each track’s entity by its identifiers, grades the match by broadcast name, and at a corroborated grade passes fields between entity and track.',
  affiliation: 'Sets affiliation from a country code, by the country lists.',
  filter: 'Keeps or drops observations after mapping and the entity stage; dropped ones are counted as filtered.',
  tracker: 'Forms tracks from detections (plots) before they leave the source.',
  throttle: 'Limits how often each source track is passed on, by time and distance moved.',
  output: 'What the source’s observations are, which decides how correlation takes them, the label they carry, and its priority.',
}

/** Drop pipeline keys set to undefined, so the saved spec carries only what is configured. */
const clean = (p: Pipeline): Pipeline => JSON.parse(JSON.stringify(p))

/** The settings of one step of a source, then its samples as they leave that step. */
export function StepPanel({
  spec,
  step,
  problems,
  schema,
  preview,
  previewError,
  probe,
  capturing,
  readOnly,
  onChange,
  onCapture,
}: {
  spec: SourceSpec
  step: StepId
  problems: string[]
  schema: SchemaOverview | null
  preview: PreviewResult | null
  previewError: string | null
  probe: ProbeResult | null
  capturing: boolean
  readOnly: boolean
  onChange: (spec: SourceSpec) => void
  onCapture: () => void
}) {
  const p = spec.pipeline as Pipeline
  const setPipeline = (next: Pipeline) => onChange({ ...spec, pipeline: clean(next) })

  let settings: React.ReactNode = null
  switch (step) {
    case 'source':
      settings = (
        <>
          <TransportForm
            part="source"
            transport={spec.transport}
            codec={spec.pipeline.codec}
            onTransport={(transport) => onChange({ ...spec, transport })}
            onCodec={(codec) => setPipeline({ ...p, codec })}
          />
          <SenderAuth spec={spec} onChange={onChange} />
        </>
      )
      break
    case 'decode': {
      const detected = probe && JSON.stringify(probe.codec) !== JSON.stringify(spec.pipeline.codec) ? probe.codec : null
      settings = (
        <>
          {detected && (
            <div className="value-row">
              <Button size="sm" variant="secondary" icon={<TbWand />} disabled={readOnly} onClick={() => setPipeline({ ...p, codec: detected })}>
                Use detected settings
              </Button>
              <InfoTip label="Detected settings">
                The captured samples decode with these settings: {JSON.stringify(detected)}. Using them replaces this step&apos;s settings.
              </InfoTip>
            </div>
          )}
          <TransportForm
            part="decode"
            transport={spec.transport}
            codec={spec.pipeline.codec}
            onTransport={(transport) => onChange({ ...spec, transport })}
            onCodec={(codec) => setPipeline({ ...p, codec })}
          />
        </>
      )
      break
    }
    case 'reject':
      settings = (
        <RejectForm
          rules={(p.mapping.reject as Obj[]) ?? []}
          onChange={(reject) => setPipeline({ ...p, mapping: { ...p.mapping, reject: reject.length ? reject : undefined } })}
        />
      )
      break
    case 'map': {
      const suggested = probe?.suggestion.mapping
      const offer = suggested && suggested.rules.length > 0 && JSON.stringify(suggested.rules) !== JSON.stringify(p.mapping.rules)
      settings = (
        <>
          {offer && (
            <div className="value-row">
              <Button
                size="sm"
                variant="secondary"
                icon={<TbWand />}
                disabled={readOnly}
                onClick={() => setPipeline({ ...p, mapping: { ...suggested, reject: p.mapping.reject } })}
              >
                Use suggested mapping
              </Button>
              <InfoTip label="Suggested mapping">
                A mapping inferred from the captured samples: {suggested.rules.length} rule(s)
                {probe.suggestion.missing.length ? `; not found in the samples: ${probe.suggestion.missing.join(', ')}` : ''}. Using it replaces
                this step&apos;s rules.
              </InfoTip>
            </div>
          )}
          {schema ? (
            <MapEditor mapping={p.mapping as Obj} schema={schema} onChange={(mapping) => setPipeline({ ...p, mapping: mapping as Pipeline['mapping'] })} />
          ) : (
            <span className="muted">LOADING…</span>
          )}
        </>
      )
      break
    }
    case 'join':
      settings = <JoinForm value={(p.static_join as Obj) ?? {}} onChange={(static_join) => setPipeline({ ...p, static_join })} />
      break
    case 'registry':
      settings = <RegistryForm value={p.registry as Obj} onChange={(registry) => setPipeline({ ...p, registry })} />
      break
    case 'affiliation':
      settings = <AffiliationForm value={p.affiliation as Obj} onChange={(affiliation) => setPipeline({ ...p, affiliation })} />
      break
    case 'filter': {
      const input = filterInput(preview?.trace)
      const fields = sampleFields([...input.observations, ...(preview?.observations ?? [])])
      const key = (o: unknown) => String((o as { source_track_key?: unknown }).source_track_key ?? '')
      const examples = [...new Set(input.dropped.map(key).filter(Boolean))].slice(0, 3)
      const effect = preview ? { dropped: preview.counts?.filtered ?? 0, examples } : null
      settings = <FilterForm value={(p.filter as Obj) ?? {}} onChange={(filter) => setPipeline({ ...p, filter })} fields={fields} effect={effect} />
      break
    }
    case 'tracker':
      settings = <TrackerForm value={p.tracker as Obj} onChange={(tracker) => setPipeline({ ...p, tracker })} />
      break
    case 'throttle':
      settings = <ThrottleForm value={p.throttle as Obj} onChange={(throttle) => setPipeline({ ...p, throttle })} />
      break
    case 'output':
      settings = (
        <div className="stack">
          <PublishForm
            reports={spec.reports}
            tracker={p.tracker !== undefined}
            publishAlone={spec.publish_alone}
            confirmAfter={spec.confirm_after}
            onChange={(reports) => {
              const { reports: _, ...rest } = spec
              onChange(reports ? { ...rest, reports } : rest)
            }}
            onPublishAlone={(publish_alone) => onChange({ ...spec, publish_alone })}
            onConfirmAfter={(confirm_after) => onChange({ ...spec, confirm_after })}
          />
          <div className="stage-row">
            <div className="row-label">
              <Label size="sm" htmlFor="source-priority">
                Priority
              </Label>
              <InfoTip label="Priority">
                Where several sources report one track, priority breaks ties: it picks the position among equally certain reports, the
                identity and classification when no report is corroborated by the registry, and each extension field. Higher wins. Default 100.
              </InfoTip>
            </div>
            <Input
              id="source-priority"
              type="number"
              style={{ width: 90 }}
              value={spec.priority === undefined ? '' : String(spec.priority)}
              placeholder="100"
              onChange={(e) => {
                const { priority: _, ...rest } = spec
                onChange(e.target.value.trim() === '' ? rest : { ...rest, priority: Math.round(Number(e.target.value)) })
              }}
            />
          </div>
          <SecurityForm value={spec.security} onChange={(security) => onChange({ ...spec, security })} />
          <EmitterMotionForm value={spec.emitter_motion} onChange={(emitter_motion) => onChange({ ...spec, emitter_motion })} />
        </div>
      )
      break
  }

  return (
    <div className="step-panel">
      <div className="stack" style={{ minWidth: 0 }}>
      <div className="value-row">
        <span className="flow-step-kind">{KIND_WORD[kindOf(step)]}</span>
        <h3 className="subhead">
          {stepTitle(spec, step)}
          <InfoTip label={stepTitle(spec, step)}>{STEP_HELP[step]}</InfoTip>
        </h3>
      </div>
      {problems.map((m) => (
        <div key={m} className="error-text" role="alert">
          {m}
        </div>
      ))}
      <fieldset className="step-settings" disabled={readOnly}>
        {settings}
      </fieldset>
      </div>
      <div className="stack step-samples">
      <div className="value-row">
        <h3 className="subhead">
          Samples
          <InfoTip label="Samples">
            A dry run of the source as edited (not yet saved) over its stored samples, as they leave this step. Nothing is published.
          </InfoTip>
        </h3>
        <span className="spacer" />
        {step === 'source' && (
          <>
            <Button size="sm" variant="secondary" icon={<TbDownload />} disabled={capturing || readOnly} onClick={onCapture}>
              {capturing ? 'Capturing…' : 'Capture samples'}
            </Button>
            <InfoTip label="Capture samples">
              Connect with this step&apos;s settings and take up to 20 frames or 15 s of the live feed, whichever comes first. They replace the
              source&apos;s stored samples.
            </InfoTip>
          </>
        )}
      </div>
      {previewError && <div className="error-text">{previewError}</div>}
      {preview && <StageSamples result={preview} stageId={traceStage(step)} />}
      </div>
    </div>
  )
}
