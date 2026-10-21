import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { TbTrash } from 'react-icons/tb'
import type { NodeChange } from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import { Badge, Breadcrumbs, Button, Input, Label, SaveButton, SegmentedControl, Toggle, useToast } from '@kineticlogic/staresdk'
import { CodeEditor } from '@kineticlogic/staresdk/code-editor'
import { FlowCanvas, type FlowCanvasEdge, type FlowCanvasNode, type FlowPaletteItem } from '@kineticlogic/staresdk/flow-canvas'
import {
  api,
  ApiError,
  type PluginInfo,
  type PreviewResult,
  type ProbeResult,
  type SchemaOverview,
  type SourceProblem,
  type SourceRow,
  type SourceSpec,
} from '../../api/client'
import { useCan } from '../../auth/context'
import { InfoTip } from '../../components/InfoTip'
import { errorMessage } from '../../lib/format'
import { pipelineStages } from '../../lib/pipeline'
import { SOURCE_STATES_INFO, sourceState } from '../../lib/sourceState'
import { MAX_TRACED_SAMPLES, TRACED_SAMPLES } from '../../lib/trace'
import { SourceHistory, SourceStatus } from './SourceStatus'
import { StepNode, type StepView } from './flow/StepNode'
import { StepPanel } from './flow/StepPanel'
import { entryIcon, stepIcon } from './flow/stepIcons'
import {
  addEntry,
  kindOf,
  paletteEntries,
  positionOf,
  problemsByStep,
  REQUIRED,
  removeStep,
  replaces,
  stepOfEntry,
  stepsOf,
  stepTitle,
  traceStage,
  type StepId,
} from './flow/sourceFlow'

const REFRESH_MS = 5000
const PREVIEW_DELAY_MS = 600
const NODE_TYPES = { step: StepNode }

/** A source's own settings: its name and description. */
function SourcePanel({ spec, row, readOnly, onChange }: { spec: SourceSpec; row: SourceRow; readOnly: boolean; onChange: (s: SourceSpec) => void }) {
  return (
    <div className="stack step-panel-single">
      <fieldset className="step-settings form-grid" disabled={readOnly}>
        <div className="field">
          <Label size="sm" htmlFor="source-name">
            Name
          </Label>
          <Input id="source-name" value={spec.name} onChange={(e) => onChange({ ...spec, name: e.target.value })} />
        </div>
        <div className="field">
          <Label size="sm" htmlFor="source-id">
            Id
          </Label>
          <Input id="source-id" value={spec.id} disabled className="mono" />
        </div>
        <div className="field wide">
          <Label size="sm" htmlFor="source-description">
            Description
          </Label>
          <Input
            id="source-description"
            value={spec.description ?? ''}
            onChange={(e) => {
              const { description: _, ...rest } = spec
              onChange(e.target.value ? { ...rest, description: e.target.value } : rest)
            }}
          />
        </div>
      </fieldset>
      <h3 className="subhead">Status</h3>
      <SourceStatus source={row} />
      <h3 className="subhead">History</h3>
      <SourceHistory id={row.id} revision={row.revision} />
    </div>
  )
}

/**
 * A dry run of the source as edited over its stored samples, re-run shortly after it stops changing:
 * what stops it running (each problem on its stage) and its samples stage by stage. A draft runs as
 * far as it can. Nothing is saved or published.
 */
function useLiveCheck(spec: SourceSpec | null, id: string, nonce: number, trace: number, enabled: boolean) {
  const [preview, setPreview] = useState<PreviewResult | null>(null)
  const [problems, setProblems] = useState<SourceProblem[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const seq = useRef(0)
  useEffect(() => {
    if (!enabled || !spec) return
    const n = ++seq.current
    const t = setTimeout(() => {
      api.preview(spec, id, [], trace).then(
        (r) => {
          if (n !== seq.current) return
          setPreview(r)
          setProblems(r.problems ?? [])
          setError(null)
        },
        (e) => {
          if (n !== seq.current) return
          setPreview(null)
          const listed = e instanceof ApiError && e.problems.length > 0
          setProblems(listed ? e.problems : null)
          setError(listed ? null : errorMessage(e))
        },
      )
    }, PREVIEW_DELAY_MS)
    return () => clearTimeout(t)
  }, [spec, id, nonce, trace, enabled])
  return { preview, problems, error }
}

/**
 * One source as a flow of steps (#104): the palette, the source's steps in pipeline order, and the
 * settings and samples of the step selected, or with none selected the source's own settings, status
 * and history. Nothing changes on the running source until Save.
 */
export default function SourceEditor({ id, onClose, onDeleted }: { id: string; onClose: () => void; onDeleted: () => void }) {
  const { confirm, toast } = useToast()
  const canAdmin = useCan('admin')
  const readOnly = !canAdmin
  const [row, setRow] = useState<SourceRow | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [spec, setSpec] = useState<SourceSpec | null>(null)
  const [baseline, setBaseline] = useState<string>('')
  const [selected, setSelected] = useState<StepId | null>(null)
  const [view, setView] = useState<'flow' | 'json'>('flow')
  const [jsonText, setJsonText] = useState('')
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const [schema, setSchema] = useState<SchemaOverview | null>(null)
  const [plugins, setPlugins] = useState<PluginInfo[]>([])
  const [probe, setProbe] = useState<ProbeResult | null>(null)
  const [capturing, setCapturing] = useState(false)
  const [nonce, setNonce] = useState(0)

  const adopt = useCallback((r: SourceRow) => {
    setRow(r)
    setSpec(r.spec)
    setBaseline(JSON.stringify(r.spec))
  }, [])

  // The row (its status, enabled, stored problems) refreshes; the spec being edited does not.
  useEffect(() => {
    let first = true
    const load = () =>
      api.source(id).then(
        (r) => {
          setLoadError(null)
          if (first) adopt(r)
          else setRow(r)
          first = false
        },
        (e) => setLoadError(errorMessage(e)),
      )
    load()
    const t = setInterval(load, REFRESH_MS)
    return () => clearInterval(t)
  }, [id, adopt])

  useEffect(() => {
    api.schema().then(setSchema, () => setSchema(null))
    api.plugins().then(setPlugins, () => setPlugins([]))
  }, [])

  const dirty = spec !== null && JSON.stringify(spec) !== baseline
  const live = useLiveCheck(spec, id, nonce, selected === 'filter' ? MAX_TRACED_SAMPLES : TRACED_SAMPLES, canAdmin)
  // What stops it running: the live check's, else what was found when it was saved.
  const problems = useMemo(() => live.problems ?? row?.problems ?? [], [live.problems, row?.problems])
  const steps = useMemo(() => (spec ? stepsOf(spec) : []), [spec])
  const byStep = useMemo(() => problemsByStep(problems, steps), [problems, steps])

  const edit = (next: SourceSpec) => {
    setSpec(next)
    setSaved(false)
    setNotice(null)
  }

  const nodes: FlowCanvasNode[] = useMemo(() => {
    if (!spec) return []
    const summaries = new Map(pipelineStages(spec).map((s) => [s.id, s.summary]))
    return steps.map((step, i) => ({
      id: step,
      type: 'step',
      position: positionOf(i),
      selected: step === selected,
      draggable: false,
      deletable: !REQUIRED.has(step),
      data: {
        kind: kindOf(step),
        icon: stepIcon(spec, step),
        title: stepTitle(spec, step),
        summary: summaries.get(traceStage(step)) ?? '',
        problems: byStep.byStep[step] ?? [],
      } satisfies StepView,
    }))
  }, [spec, steps, selected, byStep])

  const edges: FlowCanvasEdge[] = useMemo(
    () => steps.slice(1).map((step, i) => ({ id: `${steps[i]}->${step}`, source: steps[i], target: step, deletable: false })),
    [steps],
  )

  const palette: FlowPaletteItem[] = useMemo(
    () => paletteEntries(plugins).map((e) => ({ type: e.type, label: e.label, group: e.group, icon: entryIcon(e.type) })),
    [plugins],
  )

  // Steps keep their places and order. Clicking one selects it; deleting an added stage removes it
  // from the source.
  const onNodesChange = (changes: NodeChange[]) => {
    const picks = changes.flatMap((c) => (c.type === 'select' ? [c] : []))
    if (picks.length) {
      const on = picks.find((c) => c.selected)
      if (on) setSelected(on.id as StepId)
      else if (picks.some((c) => c.id === selected)) setSelected(null)
    }
    const removed = changes.flatMap((c) => (c.type === 'remove' ? [c.id as StepId] : []))
    if (removed.length && spec) {
      edit(removed.reduce(removeStep, spec))
      if (selected && removed.includes(selected)) setSelected(null)
    }
  }

  const onDropNode = async (type: string) => {
    if (!spec || readOnly) return
    const step = stepOfEntry(type)
    if (replaces(spec, type)) {
      const what = step === 'source' ? 'transport' : 'decoding'
      const label = palette.find((p) => p.type === type)?.label ?? ''
      const ok = await confirm(`Replace this source's ${what} settings with a new ${label} step?`, { title: 'Replace step', confirmLabel: 'Replace' })
      if (!ok) return
    }
    edit(addEntry(spec, type, plugins))
    if (step) setSelected(step)
  }

  const save = async () => {
    if (!spec) return
    setSaving(true)
    setNotice(null)
    try {
      adopt(await api.saveSource(spec))
      setSaved(true)
      setNonce((n) => n + 1)
    } catch (e) {
      setNotice(errorMessage(e))
    } finally {
      setSaving(false)
    }
  }

  const toggle = async (on: boolean) => {
    if (on && dirty) {
      setNotice('Save the source before turning it on.')
      return
    }
    try {
      setRow(await api.setEnabled(id, on))
      setNotice(null)
    } catch (e) {
      setNotice(errorMessage(e))
    }
  }

  const remove = async () => {
    const ok = await confirm(`Delete source ${id}? Its configuration history stays in the decision log.`, { title: 'Delete source', confirmLabel: 'Delete' })
    if (!ok) return
    try {
      await api.deleteSource(id)
      onDeleted()
    } catch (e) {
      toast({ variant: 'error', message: errorMessage(e) })
    }
  }

  const leave = async () => {
    if (dirty && !(await confirm('Leave without saving? The changes to this source are lost.', { title: 'Unsaved source', confirmLabel: 'Discard' }))) return
    onClose()
  }

  /** Take a few frames from the feed now, keep them as the source's samples, and offer what they suggest. */
  const capture = async () => {
    if (!spec) return
    setCapturing(true)
    try {
      const r = await api.probe({ transport: spec.transport, codec: spec.pipeline.codec, max_frames: 20, max_secs: 15, save_as: id })
      setProbe(r)
      if (!r.frames) toast({ variant: 'warning', message: `No frames captured: ${r.link_error ?? r.hint ?? 'none arrived in time.'}` })
      setNonce((n) => n + 1)
    } catch (e) {
      toast({ variant: 'error', title: 'Capture failed', message: errorMessage(e) })
    } finally {
      setCapturing(false)
    }
  }

  const switchView = (v: 'flow' | 'json') => {
    if (v === 'json' && spec) setJsonText(JSON.stringify(spec, null, 2))
    setView(v)
  }

  if (!spec || !row) {
    return (
      <div className="panels">
        <div className="panel-body">{loadError ? <div className="error-text">{loadError}</div> : <span className="muted">LOADING…</span>}</div>
      </div>
    )
  }

  const state = row.draft ? { label: 'draft', color: 'grey' as const } : sourceState(row)
  const general = [...(notice ? [notice] : []), ...byStep.general]

  return (
    <div className="source-editor">
      <div className="source-editor-head">
        <Breadcrumbs
          items={[{ label: 'Sources', href: '#sources' }, { label: spec.name || id }]}
          onNavigate={() => {
            void leave()
          }}
        />
        <Badge color={state.color} size="sm" uppercase>
          {state.label}
        </Badge>
        <InfoTip label="Source state">{SOURCE_STATES_INFO}</InfoTip>
        {general.length > 0 && (
          <span className="error-text source-editor-general" role="alert">
            {general.join(' · ')}
          </span>
        )}
        <span className="spacer" />
        <SegmentedControl
          aria-label="Source view"
          size="sm"
          value={view}
          onChange={switchView}
          options={[
            { value: 'flow', label: 'Flow' },
            { value: 'json', label: 'JSON' },
          ]}
        />
        <label className="row" style={{ gap: 6 }}>
          <span className="muted">Enabled</span>
          <Toggle size="sm" value={row.enabled} disabled={readOnly} onChange={toggle} aria-label="Enabled" />
        </label>
        {canAdmin && <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />}
        <Button size="xs" variant="ghost" icon={<TbTrash />} disabled={readOnly} aria-label="Delete source" title="Delete source" onClick={remove} />
      </div>
      {view === 'flow' ? (
        <div className="source-editor-body">
          <FlowCanvas
            aria-label={`Steps of ${spec.name || id}`}
            nodes={nodes}
            edges={edges}
            nodeTypes={NODE_TYPES}
            onNodesChange={onNodesChange}
            isValidConnection={() => false}
            palette={palette}
            onDropNode={(type) => void onDropNode(type)}
            readOnly={readOnly}
            height="100%"
            style={{ minHeight: 0 }}
          />
          <aside className="source-editor-aside">
            {selected ? (
              <StepPanel
                spec={spec}
                step={selected}
                problems={byStep.byStep[selected] ?? []}
                schema={schema}
                preview={live.preview}
                previewError={live.error}
                probe={probe}
                capturing={capturing}
                readOnly={readOnly}
                onChange={edit}
                onCapture={() => void capture()}
              />
            ) : (
              <SourcePanel spec={spec} row={row} readOnly={readOnly} onChange={edit} />
            )}
          </aside>
        </div>
      ) : (
        <div className="source-editor-json">
          <CodeEditor
            aria-label="Source settings as JSON"
            value={jsonText}
            readOnly={readOnly}
            onChange={(t) => {
              setJsonText(t)
              try {
                const next = JSON.parse(t) as SourceSpec
                if (next && typeof next === 'object' && next.id === id) edit(next)
              } catch {
                // Keep the last valid settings; the editor marks the parse error.
              }
            }}
          />
        </div>
      )}
    </div>
  )
}
