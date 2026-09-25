import { lazy, Suspense, useEffect, useMemo, useState } from 'react'
import { TbAlertTriangle, TbPencil } from 'react-icons/tb'
import { Badge, Button, DataTable, TabPanel, Tabs, type DataTableColumn } from 'staresdk'
import { api, type SchemaOverview, type SourceSpec } from '../../api/client'
import { errorMessage } from '../../lib/format'
import { fieldMap, pipelineStages, ruleNotes, type FieldRow } from '../../lib/pipeline'
import { MappingStudio } from './MappingStudio'
import { PipelineDesigner } from './designer/PipelineDesigner'

// React Flow loads only when the flowchart is shown.
const PipelineFlow = lazy(() => import('./PipelineFlow'))

const VIEWS = [
  { id: 'flow', label: 'Flow' },
  { id: 'json', label: 'JSON' },
]

const COLUMNS: DataTableColumn<FieldRow>[] = [
  { key: 'stage', header: 'Set by', width: 110, render: (r) => r.stage, sortValue: (r) => r.stage },
  {
    key: 'field',
    header: 'Feed field → OpenTrack field',
    render: (r) => (
      <div className="flowcell">
        <span className="mono">
          {r.from} <span className="muted">→</span> {r.target}
        </span>
        <span className="muted">{r.how}</span>
      </div>
    ),
    sortValue: (r) => r.target,
  },
  {
    key: 'published',
    header: 'Published as',
    render: (r) => (
      <span className="dest">
        {r.published.map((d) =>
          d.kind === 'attribute' ? (
            <Badge key={d.text} color="blue" size="sm">
              {d.text}
            </Badge>
          ) : d.kind === 'gold' ? (
            <Badge key={d.text} color="grey" size="sm" title="Always-published OTH-GOLD field">
              GOLD {d.text}
            </Badge>
          ) : d.kind === 'none' ? (
            <span key={d.text} className="notice" style={{ margin: 0 }}>
              <TbAlertTriangle aria-hidden /> {d.text}
            </span>
          ) : (
            <span key={d.text} className="muted">
              {d.text}
            </span>
          ),
        )}
      </span>
    ),
    sortValue: (r) => (r.published.some((d) => d.kind === 'none') ? 0 : 1),
  },
]

function MapDetail({ spec, schema }: { spec: SourceSpec; schema: SchemaOverview }) {
  const rows = useMemo(() => fieldMap(spec, schema), [spec, schema])
  const notes = useMemo(() => ruleNotes(spec), [spec])
  // Values set but never published, by field (several rules can set the same one).
  const lost = useMemo(() => {
    const by = new Map<string, Set<string>>()
    for (const r of rows.filter((r) => r.published.some((d) => d.kind === 'none'))) {
      const from = by.get(r.target) ?? new Set<string>()
      if (r.from) from.add(r.from)
      by.set(r.target, from)
    }
    return [...by].map(([target, from]) => `${target} (from ${[...from].join(' / ')})`)
  }, [rows])
  return (
    <div className="stack">
      {lost.length > 0 && (
        <div className="notice" style={{ margin: 0 }}>
          <TbAlertTriangle aria-hidden /> {lost.length} value{lost.length === 1 ? ' is' : 's are'} set but never published:{' '}
          {lost.join(', ')}. Add or link a field in the Schema workspace to
          publish {lost.length === 1 ? 'it' : 'them'}.
        </div>
      )}
      {notes.some((n) => n.notes.length > 0) && (
        <dl className="facts">
          {notes
            .filter((n) => n.notes.length > 0)
            .map((n) => (
              <div key={n.stage} style={{ display: 'contents' }}>
                <dt>{n.stage}</dt>
                <dd>
                  {n.notes.map((t) => (
                    <div key={t} className="mono">
                      {t}
                    </div>
                  ))}
                </dd>
              </div>
            ))}
        </dl>
      )}
      <DataTable
        aria-label="Field map"
        columns={COLUMNS}
        rows={rows}
        rowKey={(r) => r.id}
        maxHeight={520}
        empty="This mapping sets no fields."
      />
    </div>
  )
}

/**
 * A source's pipeline: a top-to-bottom flowchart of its stages with the selected stage's detail
 * (the Map stage traces every field from the feed to the published message), or the raw JSON
 * with a live preview.
 */
export function PipelineView({
  spec,
  onChange,
  onSaved,
  sourceId,
}: {
  spec: SourceSpec
  onChange: (s: SourceSpec) => void
  /** The designer saved a new revision with this spec. */
  onSaved: (s: SourceSpec) => void
  sourceId: string
}) {
  const [designing, setDesigning] = useState(false)
  const [view, setView] = useState('flow')
  const [stageId, setStageId] = useState('map')
  const [schema, setSchema] = useState<SchemaOverview | null>(null)
  const [schemaError, setSchemaError] = useState<string | null>(null)

  useEffect(() => {
    api.schema().then(setSchema, (e) => setSchemaError(errorMessage(e)))
  }, [])

  const stages = useMemo(() => pipelineStages(spec), [spec])
  const stage = stages.find((s) => s.id === stageId) ?? stages[0]

  return (
    <div className="stack">
      <div className="value-row">
        <Tabs aria-label="Pipeline views" idPrefix="pipe" size="sm" value={view} onChange={setView} tabs={VIEWS} />
        <span className="spacer" />
        <Button size="sm" variant="secondary" icon={<TbPencil />} onClick={() => setDesigning(true)}>
          Edit
        </Button>
      </div>
      {designing && (
        <PipelineDesigner
          initial={spec}
          sourceId={sourceId}
          onClose={() => setDesigning(false)}
          onSaved={(s) => {
            onSaved(s)
            setDesigning(false)
          }}
        />
      )}
      <TabPanel id={view} idPrefix="pipe">
        {view === 'json' ? (
          <MappingStudio spec={spec} onChange={onChange} sampleSourceId={sourceId} showMap={false} />
        ) : (
          <div className="pipeline">
            <Suspense fallback={<span className="muted">LOADING…</span>}>
              <PipelineFlow stages={stages} selected={stage.id} onSelect={setStageId} />
            </Suspense>
            <div className="stack" style={{ minWidth: 0 }}>
              <h3 className="subhead">{stage.title}</h3>
              {stage.id === 'map' ? (
                schema ? (
                  <MapDetail spec={spec} schema={schema} />
                ) : (
                  <span className={schemaError ? 'error-text' : 'muted'}>{schemaError ?? 'LOADING…'}</span>
                )
              ) : (
                <dl className="facts">
                  {stage.facts.map((f) => (
                    <div key={f.label} style={{ display: 'contents' }}>
                      <dt>{f.label}</dt>
                      <dd className="mono">{f.value}</dd>
                    </div>
                  ))}
                </dl>
              )}
            </div>
          </div>
        )}
      </TabPanel>
    </div>
  )
}
