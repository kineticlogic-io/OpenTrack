import { useCallback, useEffect, useMemo, useState } from 'react'
import { TbPencil, TbSend, TbTrash } from 'react-icons/tb'
import { Badge, Button, DataTable, SaveButton, useToast, type DataTableColumn } from 'staresdk'
import { CodeEditor } from 'staresdk/code-editor'
import { api, type ExtensionField, type SchemaOverview, type SchemaVersion } from '../../api/client'
import { errorMessage, fmtTime } from '../../lib/format'

type VersionRow = SchemaVersion & { sources: string[] }

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
  { key: 'sources', header: 'Used by', render: (v) => v.sources.join(', ') || <span className="muted">none</span> },
  { key: 'published', header: 'Published', mono: true, width: 150, render: (v) => (v.published_at_ms ? fmtTime(v.published_at_ms) : '—') },
]

const FIELD_COLUMNS: DataTableColumn<ExtensionField>[] = [
  { key: 'key', header: 'Key', mono: true, width: '22%', render: (f) => `ext.${f.key}`, sortValue: (f) => f.key },
  { key: 'type', header: 'Type', width: '12%', render: (f) => f.type, sortValue: (f) => f.type },
  { key: 'unit', header: 'Unit', width: '8%', render: (f) => f.unit ?? '' },
  { key: 'req', header: 'Required', width: '10%', render: (f) => (f.required ? 'yes' : '') },
  {
    key: 'default',
    header: 'Default',
    mono: true,
    width: '12%',
    render: (f) => (f.default === undefined ? '' : JSON.stringify(f.default)),
  },
  { key: 'desc', header: 'Description', render: (f) => f.description ?? (f.enum_values ? f.enum_values.join(' | ') : '') },
]

export default function SchemaPage() {
  const { toast, confirm } = useToast()
  const [overview, setOverview] = useState<SchemaOverview | null>(null)
  const [selected, setSelected] = useState<number | null>(null)
  const [text, setText] = useState('')
  const [editing, setEditing] = useState(false)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    const o = await api.schema()
    setOverview(o)
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
  // Until the user picks one, show the newest version.
  const shownVersion = selected ?? rows[rows.length - 1]?.version ?? null
  const current = rows.find((v) => v.version === shownVersion) ?? null
  const latestPublished = [...rows].reverse().find((v) => v.status === 'published')

  const startEditing = () => {
    const base = draft ?? latestPublished
    setText(JSON.stringify(base?.fields ?? [], null, 2))
    setEditing(true)
    setError(null)
  }

  const saveDraft = async () => {
    setSaving(true)
    setError(null)
    try {
      const fields = JSON.parse(text) as ExtensionField[]
      const d = await api.saveDraft(fields, draft?.notes ?? undefined)
      await load()
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
    const ok = await confirm(`Publish schema version ${draft.version}? A published version can never change.`, {
      title: 'Publish schema',
      confirmLabel: 'Publish',
    })
    if (!ok) return
    try {
      const p = await api.publishDraft()
      setEditing(false)
      await load()
      setSelected(p.version)
      toast({ variant: 'success', message: `Schema version ${p.version} published.` })
    } catch (e) {
      setError(errorMessage(e))
    }
  }

  const discard = async () => {
    try {
      await api.discardDraft()
      setEditing(false)
      const o = await load()
      setSelected(o.latest_published)
    } catch (e) {
      setError(errorMessage(e))
    }
  }

  const draftDirty = editing && text !== JSON.stringify(draft?.fields ?? latestPublished?.fields ?? [], null, 2)

  return (
    <div className="split">
      <div className="stack">
        <section className="section" aria-labelledby="versions-heading">
          <div className="section-head">
            <h2 id="versions-heading">Schema versions</h2>
            <span className="spacer" />
            {!editing && (
              <Button size="sm" icon={<TbPencil />} onClick={startEditing}>
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
            empty="Loading…"
          />
        </section>
        <section className="section" aria-labelledby="core-heading">
          <h2 id="core-heading">Core fields</h2>
          <p className="muted" style={{ margin: '0 0 8px' }}>
            Fixed: correlation and peat-node depend on them. Extensions are mapped as <span className="mono">ext.&lt;key&gt;</span>.
          </p>
          <div className="counts">
            {(overview?.core ?? []).map((f) => (
              <Badge key={f} color="grey" size="sm">
                {f}
              </Badge>
            ))}
          </div>
        </section>
      </div>

      <section className="section" aria-labelledby="fields-heading">
        <div className="section-head">
          <h2 id="fields-heading">{editing ? `Draft version ${draft?.version ?? (latestPublished ? latestPublished.version + 1 : 1)}` : current ? `Version ${current.version}` : 'Fields'}</h2>
          {current?.notes && !editing && <span className="muted">{current.notes}</span>}
          <span className="spacer" />
          {editing && (
            <>
              <SaveButton size="sm" dirty={draftDirty} saving={saving} saved={saved} onSave={saveDraft} />
              {draft && (
                <Button size="sm" variant="secondary" icon={<TbSend />} onClick={publish} disabled={draftDirty}>
                  Publish
                </Button>
              )}
              {draft ? (
                <Button size="xs" variant="ghost" icon={<TbTrash />} aria-label="Discard draft" title="Discard draft" onClick={discard} />
              ) : (
                <Button size="sm" variant="ghost" onClick={() => setEditing(false)}>
                  Cancel
                </Button>
              )}
            </>
          )}
        </div>
        {error && (
          <div className="error-text" style={{ marginBottom: 8 }}>
            {error}
          </div>
        )}
        {editing ? (
          <div className="stack" style={{ gap: 6 }}>
            <span className="muted">
              Each field: <span className="mono">key</span>, <span className="mono">type</span> (string, integer, number,
              boolean, enum, timestamp, position, json), and optionally unit, required, default, enum_values,
              description. Save the draft, then publish it.
            </span>
            <CodeEditor aria-label="Draft fields" value={text} onChange={setText} minHeight={240} maxHeight={560} />
          </div>
        ) : (
          <DataTable
            aria-label="Extension fields"
            columns={FIELD_COLUMNS}
            rows={current?.fields ?? []}
            rowKey={(f) => f.key}
            empty={current?.version === 1 ? 'Version 1 is the core schema; it has no extension fields.' : 'No extension fields.'}
          />
        )}
      </section>
    </div>
  )
}
