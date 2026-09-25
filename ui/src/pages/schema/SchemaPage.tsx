import { useCallback, useEffect, useMemo, useState } from 'react'
import { TbPencil, TbPlus, TbSend, TbTrash } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, SaveButton, TabPanel, Tabs, useToast, type DataTableColumn } from 'staresdk'
import { CodeEditor } from 'staresdk/code-editor'
import { api, type ExtensionField, type SchemaOverview, type SchemaVersion, type SourceRow } from '../../api/client'
import { errorMessage, fmtTime } from '../../lib/format'
import { unpublished, usedBy, type Unpublished } from '../../lib/schemaUsage'
import { FieldForm } from './FieldForm'

type VersionRow = SchemaVersion & { sources: string[] }
type FieldRow = ExtensionField & { used: string[] }

const VERSION_COLUMNS: DataTableColumn<VersionRow>[] = [
  { key: 'version', header: 'Version', width: 72, render: (v) => v.version, sortValue: (v) => v.version },
  {
    key: 'status',
    header: 'Status',
    width: 96,
    render: (v) => (
      <Badge color={v.status === 'draft' ? 'warning' : 'grey'} size="sm" uppercase>
        {v.status}
      </Badge>
    ),
  },
  { key: 'fields', header: 'Fields', width: 64, align: 'right', render: (v) => v.fields.length },
  { key: 'sources', header: 'Mappings validated against it', render: (v) => v.sources.join(', ') || <span className="muted">none</span> },
  { key: 'published', header: 'Published', mono: true, width: 150, render: (v) => (v.published_at_ms ? fmtTime(v.published_at_ms) : '—') },
]

const FIELD_COLUMNS: DataTableColumn<FieldRow>[] = [
  { key: 'key', header: 'Field', mono: true, width: '18%', render: (f) => f.key, sortValue: (f) => f.key },
  { key: 'type', header: 'Type', width: '9%', render: (f) => f.type, sortValue: (f) => f.type },
  { key: 'unit', header: 'Unit', width: '6%', render: (f) => f.unit ?? '' },
  {
    key: 'source',
    header: 'Filled by',
    width: '18%',
    render: (f) =>
      f.builtin ? (
        <span>
          OpenTrack <span className="mono">{f.builtin}</span>
        </span>
      ) : (
        <span className="muted">card, else feed mapping</span>
      ),
    sortValue: (f) => f.builtin ?? '',
  },
  {
    key: 'used',
    header: 'Fed by',
    width: '16%',
    render: (f) => (f.builtin ? <span className="muted">—</span> : f.used.join(', ') || <span className="muted">cards only</span>),
  },
  { key: 'desc', header: 'Notes', render: (f) => f.description ?? (f.enum_values ? f.enum_values.join(' | ') : '') },
]

const MODES = [
  { id: 'form', label: 'Form' },
  { id: 'json', label: 'JSON' },
]

const asJson = (f: ExtensionField[]) => JSON.stringify(f, null, 2)

export default function SchemaPage() {
  const { toast, confirm } = useToast()
  const [overview, setOverview] = useState<SchemaOverview | null>(null)
  const [sources, setSources] = useState<SourceRow[]>([])
  const [selected, setSelected] = useState<number | null>(null)
  // The draft being edited (null when not editing), as fields and as JSON text.
  const [fields, setFields] = useState<ExtensionField[] | null>(null)
  const [mode, setMode] = useState('form')
  const [text, setText] = useState('')
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    const [o, s] = await Promise.all([api.schema(), api.sources()])
    setOverview(o)
    setSources(s)
    return o
  }, [])

  useEffect(() => {
    // Fetch on mount: an external system (the API), which is what effects are for.
    // eslint-disable-next-line react/set-state-in-effect
    load().catch((e) => setError(errorMessage(e)))
  }, [load])

  const rows: VersionRow[] = useMemo(
    () =>
      (overview?.versions ?? []).map((v) => ({
        ...v,
        sources: (overview?.sources ?? []).filter((s) => s.schema_version === v.version).map((s) => s.source),
      })),
    [overview],
  )
  const draft = rows.find((v) => v.status === 'draft')
  const latestPublished = [...rows].reverse().find((v) => v.status === 'published')
  // Until the user picks one, show the newest version.
  const shownVersion = selected ?? rows[rows.length - 1]?.version ?? null
  const current = rows.find((v) => v.version === shownVersion) ?? null
  const editing = fields !== null

  const usage = useMemo(() => {
    const out: Record<string, string[]> = {}
    if (!overview) return out
    const keys = new Set([...(current?.fields ?? []), ...(fields ?? [])].map((f) => f.key))
    for (const k of keys) out[k] = usedBy(k, sources, overview)
    return out
  }, [overview, sources, current, fields])
  const missing = useMemo(() => (overview ? unpublished(sources, overview) : []), [overview, sources])

  const baseFields = () => draft?.fields ?? latestPublished?.fields ?? []
  const startEditing = (extra: ExtensionField[] = []) => {
    const next = [...(fields ?? baseFields()), ...extra]
    setFields(next)
    setText(asJson(next))
    setError(null)
  }
  const cancel = () => {
    setFields(null)
    setError(null)
  }

  const switchMode = (m: string) => {
    if (m === 'json' && fields) setText(asJson(fields))
    if (m === 'form') {
      try {
        setFields(JSON.parse(text) as ExtensionField[])
      } catch (e) {
        setError(`The JSON does not parse: ${errorMessage(e)}`)
        return
      }
    }
    setError(null)
    setMode(m)
  }

  /** The fields being edited, from whichever view is showing. */
  const edited = (): ExtensionField[] => (mode === 'json' ? (JSON.parse(text) as ExtensionField[]) : (fields ?? []))

  const saveDraft = async () => {
    setSaving(true)
    setError(null)
    try {
      const f = edited()
      const d = await api.saveDraft(f, draft?.notes ?? undefined)
      await load()
      setFields(d.fields)
      setText(asJson(d.fields))
      setSelected(d.version)
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setSaving(false)
    }
  }

  const publish = async () => {
    if (!draft) return
    const ok = await confirm(
      `Publish schema version ${draft.version}? A published version can never change, and every track is republished with its attributes.`,
      { title: 'Publish schema', confirmLabel: 'Publish' },
    )
    if (!ok) return
    try {
      const p = await api.publishDraft()
      setFields(null)
      await load()
      setSelected(p.version)
      toast({ variant: 'success', message: `Schema version ${p.version} published.` })
    } catch (e) {
      setError(errorMessage(e))
    }
  }

  const discard = async () => {
    const ok = await confirm(`Discard draft version ${draft?.version}? Its changes are lost.`, { title: 'Discard draft', confirmLabel: 'Discard' })
    if (!ok) return
    try {
      await api.discardDraft()
      setFields(null)
      const o = await load()
      setSelected(o.latest_published)
    } catch (e) {
      setError(errorMessage(e))
    }
  }

  const addMissing = (u: Unpublished) => {
    if ((fields ?? baseFields()).some((f) => f.key === u.suggestion.key)) {
      toast({ variant: 'warning', message: `The draft already has a field named ${u.suggestion.key}; rename one of them.` })
    }
    startEditing([u.suggestion])
    setMode('form')
  }

  let dirty = false
  if (editing) {
    try {
      dirty = asJson(edited()) !== asJson(draft?.fields ?? latestPublished?.fields ?? [])
    } catch {
      dirty = true
    }
  }
  const draftNumber = draft?.version ?? (latestPublished ? latestPublished.version + 1 : 1)
  const fieldsTitle = editing ? `Draft version ${draftNumber}` : current ? `Version ${current.version} fields` : 'Fields'

  return (
    <div className="panels">
      <CollapsiblePanel title="Output schema versions" badge={rows.length ? String(rows.length) : undefined} persistKey="ot.panel.schemaVersions">
        <div className="panel-body">
          <div className="toolbar">
            <span className="muted">
              The output schema defines the attributes every published track carries and the fields of every card. A
              published version never changes; edit a draft, then publish it.
            </span>
            <span className="spacer" />
            {!editing && (
              <Button size="sm" icon={<TbPencil />} onClick={() => startEditing()}>
                {draft ? 'Edit draft' : 'New draft'}
              </Button>
            )}
          </div>
          <DataTable
            aria-label="Schema versions"
            columns={VERSION_COLUMNS}
            rows={rows}
            rowKey={(v) => String(v.version)}
            selectedKey={shownVersion === null ? null : String(shownVersion)}
            onRowClick={(v) => setSelected(v.version)}
            empty="LOADING…"
          />
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel
        title={fieldsTitle}
        badge={current?.notes && !editing ? current.notes : undefined}
        persistKey="ot.panel.schemaFields"
        actions={
          editing && (
            <>
              <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={saveDraft} />
              {draft && (
                <Button size="sm" variant="secondary" icon={<TbSend />} onClick={publish} disabled={dirty}>
                  Publish
                </Button>
              )}
              {draft ? (
                <Button size="xs" variant="ghost" icon={<TbTrash />} aria-label="Discard draft" title="Discard draft" onClick={discard} />
              ) : (
                <Button size="sm" variant="ghost" onClick={cancel}>
                  Cancel
                </Button>
              )}
            </>
          )
        }
      >
        <div className="panel-body">
          {error && <div className="error-text">{error}</div>}
          {editing && overview ? (
            <>
              <Tabs aria-label="Draft views" idPrefix="schema" size="sm" value={mode} onChange={switchMode} tabs={MODES} />
              <TabPanel id={mode} idPrefix="schema">
                {mode === 'form' ? (
                  <FieldForm fields={fields ?? []} onChange={setFields} schema={overview} usage={usage} />
                ) : (
                  <CodeEditor aria-label="Draft fields" value={text} onChange={setText} minHeight={240} maxHeight={560} />
                )}
              </TabPanel>
              {!draft && <span className="muted">Save to create draft version {draftNumber}; nothing is published until you publish it.</span>}
            </>
          ) : (
            <DataTable
              aria-label="Output schema fields"
              columns={FIELD_COLUMNS}
              rows={(current?.fields ?? []).map((f) => ({ ...f, used: usage[f.key] ?? [] }))}
              rowKey={(f) => f.key}
              empty={current?.version === 1 ? 'Version 1 has no fields: tracks publish only the always-published set.' : 'No fields.'}
            />
          )}
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel title="Set by feeds, not published" badge={missing.length ? String(missing.length) : undefined} persistKey="ot.panel.schemaMissing">
        <div className="panel-body">
          <span className="muted">
            Values enabled sources set that no field of version {latestPublished?.version ?? '—'} publishes. Add one to
            the draft to publish it once the draft is published.
          </span>
          <DataTable
            aria-label="Values not published"
            columns={[
              { key: 'target', header: 'OpenTrack field', mono: true, width: '24%', render: (u) => u.target, sortValue: (u) => u.target },
              { key: 'from', header: 'Set by', mono: true, render: (u) => u.from.join(' · ') },
              {
                key: 'as',
                header: 'Would publish as',
                width: '26%',
                render: (u) => (
                  <span className="mono">
                    attributes.{u.suggestion.key}
                    {u.suggestion.builtin && <span className="muted"> (built-in {u.suggestion.builtin})</span>}
                  </span>
                ),
              },
              {
                key: 'add',
                header: '',
                width: 110,
                align: 'right',
                render: (u) => (
                  <Button size="sm" variant="secondary" icon={<TbPlus />} onClick={() => addMissing(u)}>
                    Add
                  </Button>
                ),
              },
            ]}
            rows={missing}
            rowKey={(u) => u.target}
            empty={overview ? 'Everything the enabled sources set is published.' : 'LOADING…'}
          />
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel title="Always published" persistKey="ot.panel.schemaCore">
        <div className="panel-body">
          <span className="muted">
            The OTH-GOLD minimum (contact and position sets) and a symbol code (SIDC) are in every track message. The
            output schema adds <span className="mono">attributes</span>: each field is filled from the entity's card,
            else from a feed mapping (<span className="mono">ext.&lt;field&gt;</span>), or linked to an OpenTrack value.
          </span>
          <dl className="facts">
            {(overview?.published_core ?? []).map((f) => (
              <div key={f} style={{ display: 'contents' }}>
                <dt className="mono">{f}</dt>
                <dd className="mono muted">{(overview?.gold_sources?.[f] ?? []).join(' → ') || 'computed by OpenTrack'}</dd>
              </div>
            ))}
          </dl>
        </div>
      </CollapsiblePanel>
    </div>
  )
}
