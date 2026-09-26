import { lazy, Suspense, useEffect, useMemo, useState } from 'react'
import { TbDownload, TbPlus, TbTrash } from 'react-icons/tb'
import { Button, Modal, SaveButton, useToast } from 'staresdk'
import { api, type SchemaOverview, type SourceSpec } from '../../../api/client'
import { errorMessage } from '../../../lib/format'
import { pipelineStages, DEFAULT_LINKS } from '../../../lib/pipeline'
import { usePreview } from '../../../lib/usePreview'
import { PreviewResults } from '../PreviewResults'
import { MapEditor } from './MapEditor'
import { AffiliationForm, DecodeForm, FilterForm, JoinForm, PublishForm, RegistryForm, RejectForm, ThrottleForm, TrackerForm, SecurityForm } from './StageForms'

const PipelineFlow = lazy(() => import('../PipelineFlow'))

type Obj = Record<string, unknown>
type Pipeline = SourceSpec['pipeline'] & Obj

/** Optional stages the user can add, with what a new one starts as. */
const OPTIONAL: { id: string; label: string; add: (p: Pipeline) => Pipeline; remove: (p: Pipeline) => Pipeline }[] = [
  {
    id: 'reject',
    label: 'Reject',
    add: (p) => ({ ...p, mapping: { ...p.mapping, reject: [{ reason: 'no_position', when: { path: 'lat', exists: false } }] } }),
    remove: (p) => ({ ...p, mapping: { ...p.mapping, reject: undefined } }),
  },
  {
    id: 'registry',
    label: 'Entity links',
    add: (p) => ({ ...p, registry: { links: DEFAULT_LINKS } }),
    remove: (p) => ({ ...p, registry: undefined }),
  },
  {
    id: 'affiliation',
    label: 'Affiliation',
    add: (p) => ({ ...p, affiliation: { country: 'platform.flag', friendly: [], hostile: [], neutral: [] } }),
    remove: (p) => ({ ...p, affiliation: undefined }),
  },
  {
    id: 'filter',
    label: 'Filter',
    add: (p) => ({ ...p, filter: {} }),
    remove: (p) => ({ ...p, filter: undefined }),
  },
  {
    id: 'tracker',
    label: 'Tracker',
    add: (p) => ({ ...p, tracker: { algorithm: 'gnn' } }),
    remove: (p) => ({ ...p, tracker: undefined }),
  },
  {
    id: 'throttle',
    label: 'Throttle',
    add: (p) => ({ ...p, throttle: { min_interval_secs: 5, min_move_m: 0, heartbeat_secs: 600 } }),
    remove: (p) => ({ ...p, throttle: undefined }),
  },
]

const present = (p: Pipeline, id: string) =>
  id === 'reject' ? ((p.mapping.reject as unknown[]) ?? []).length > 0 : id === 'registry' ? p.registry !== undefined : p[id] !== undefined

/** Drop keys set to undefined, so the saved spec only carries what is configured. */
const clean = (p: Pipeline): Pipeline => JSON.parse(JSON.stringify(p))

/**
 * Design a source's pipeline stage by stage, with a live dry run over the stored samples. Saving
 * stores a new revision of the source (the worker restarts with it); nothing changes until then.
 */
export function PipelineDesigner({
  initial,
  sourceId,
  onClose,
  onSaved,
}: {
  initial: SourceSpec
  sourceId: string
  onClose: () => void
  onSaved: (spec: SourceSpec) => void
}) {
  const { confirm } = useToast()
  const [spec, setSpec] = useState<SourceSpec>(initial)
  const [stageId, setStageId] = useState('map')
  const [schema, setSchema] = useState<SchemaOverview | null>(null)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [nonce, setNonce] = useState(0)
  const [capturing, setCapturing] = useState(false)
  const { preview, error: previewError, running } = usePreview(spec, sourceId, nonce)

  useEffect(() => {
    api.schema().then(setSchema, (e) => setError(errorMessage(e)))
  }, [])

  const p = spec.pipeline as Pipeline
  const setPipeline = (next: Pipeline) => setSpec({ ...spec, pipeline: clean(next) })
  const stages = useMemo(() => pipelineStages(spec), [spec])
  const stage = stages.find((s) => s.id === stageId) ?? stages.find((s) => s.id === 'map')!
  const optional = OPTIONAL.find((o) => o.id === stage.id)
  const dirty = JSON.stringify(spec) !== JSON.stringify(initial)

  /** Take a few frames from the feed now and keep them as this source's samples. */
  const capture = async () => {
    setCapturing(true)
    setError(null)
    try {
      const r = await api.probe({ transport: spec.transport, codec: spec.pipeline.codec, max_frames: 20, max_secs: 15, save_as: sourceId })
      if (r.link_error && !r.frames) setError(`No frames captured: ${r.link_error}`)
      setNonce((n) => n + 1)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setCapturing(false)
    }
  }

  const save = async () => {
    setSaving(true)
    setError(null)
    try {
      await api.saveSource(spec)
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
      onSaved(spec)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setSaving(false)
    }
  }

  const close = async () => {
    if (dirty && !(await confirm('Close without saving? The pipeline changes are lost.', { title: 'Unsaved pipeline', confirmLabel: 'Discard' }))) return
    onClose()
  }

  let editor: React.ReactNode = null
  switch (stage.id) {
    case 'transport':
      editor = <span className="muted">The transport is set on the source's Transport tab.</span>
      break
    case 'decode':
      editor = <DecodeForm value={p.codec as Obj} onChange={(codec) => setPipeline({ ...p, codec: codec as Pipeline['codec'] })} />
      break
    case 'reject':
      editor = <RejectForm rules={(p.mapping.reject as Obj[]) ?? []} onChange={(reject) => setPipeline({ ...p, mapping: { ...p.mapping, reject: reject.length ? reject : undefined } })} />
      break
    case 'map':
      editor = schema ? (
        <MapEditor mapping={p.mapping as Obj} schema={schema} onChange={(mapping) => setPipeline({ ...p, mapping: mapping as Pipeline['mapping'] })} />
      ) : (
        <span className="muted">LOADING…</span>
      )
      break
    case 'join':
      editor = <JoinForm value={(p.static_join as Obj) ?? {}} onChange={(static_join) => setPipeline({ ...p, static_join })} />
      break
    case 'registry':
      editor = p.registry ? (
        <RegistryForm value={p.registry as Obj} onChange={(registry) => setPipeline({ ...p, registry })} />
      ) : (
        <span className="muted">
          Runs with defaults: resolves identifiers, and at a corroborated match the entity&apos;s name, class name, domain, affiliation, track type
          and symbol populate the track. Configure it to choose the links.
        </span>
      )
      break
    case 'affiliation':
      editor = <AffiliationForm value={p.affiliation as Obj} onChange={(affiliation) => setPipeline({ ...p, affiliation })} />
      break
    case 'filter':
      editor = <FilterForm value={p.filter as Obj} onChange={(filter) => setPipeline({ ...p, filter })} />
      break
    case 'tracker':
      editor = <TrackerForm value={p.tracker as Obj} onChange={(tracker) => setPipeline({ ...p, tracker })} />
      break
    case 'throttle':
      editor = <ThrottleForm value={p.throttle as Obj} onChange={(throttle) => setPipeline({ ...p, throttle })} />
      break
    case 'publish':
      editor = (
        <PublishForm
          reports={spec.reports}
          tracker={p.tracker !== undefined}
          publishAlone={spec.publish_alone}
          onChange={(reports) => setSpec({ ...spec, reports })}
          onPublishAlone={(publish_alone) => setSpec({ ...spec, publish_alone })}
        />
      )
      editor = (
        <div className="stack">
          {editor}
          <SecurityForm value={spec.security} onChange={(security) => setSpec({ ...spec, security })} />
        </div>
      )
      break
  }

  return (
    <Modal
      title={`Pipeline · ${spec.name}`}
      width="94vw"
      onClose={close}
      titleActions={<SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />}
    >
      <div className="designer">
        <div className="stack">
          <Suspense fallback={<span className="muted">LOADING…</span>}>
            <PipelineFlow stages={stages} selected={stage.id} onSelect={setStageId} />
          </Suspense>
          <div className="counts">
            {OPTIONAL.filter((o) => !present(p, o.id)).map((o) => (
              <Button
                key={o.id}
                size="sm"
                variant="secondary"
                icon={<TbPlus />}
                onClick={() => {
                  setPipeline(o.add(p))
                  setStageId(o.id)
                }}
              >
                {o.label}
              </Button>
            ))}
          </div>
        </div>
        <div className="stack" style={{ minWidth: 0 }}>
          <div className="value-row">
            <h3 className="subhead">{stage.title}</h3>
            <span className="spacer" />
            {optional && present(p, optional.id) && (
              <Button
                size="sm"
                variant="ghost"
                icon={<TbTrash />}
                onClick={() => {
                  setPipeline(optional.remove(p))
                  setStageId('map')
                }}
              >
                {optional.id === 'registry' ? 'Use defaults' : 'Remove stage'}
              </Button>
            )}
          </div>
          {error && <div className="error-text">{error}</div>}
          {editor}
        </div>
        <div className="stack designer-preview">
          <div className="value-row">
            <h3 className="subhead">Live preview</h3>
            <span className="muted">{running ? 'running…' : `stored samples of ${sourceId}`}</span>
            <span className="spacer" />
            <Button size="sm" variant="secondary" icon={<TbDownload />} disabled={capturing} onClick={capture}>
              {capturing ? 'Capturing…' : 'Capture samples'}
            </Button>
          </div>

          {previewError && <div className="error-text">{previewError}</div>}
          {preview ? <PreviewResults result={preview} showMap={false} /> : !previewError && <span className="muted">Running the first preview…</span>}
        </div>
      </div>
    </Modal>
  )
}
