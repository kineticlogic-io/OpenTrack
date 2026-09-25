import { useCallback, useEffect, useMemo, useState } from 'react'
import { TbPencil, TbSend, TbTrash } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, SaveButton, useToast, type DataTableColumn } from 'staresdk'
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
  { key: 'key', header: 'Field', mono: true, width: '20%', render: (f) => f.key, sortValue: (f) => f.key },
  { key: 'type', header: 'Type', width: '10%', render: (f) => f.type, sortValue: (f) => f.type },
  { key: 'unit', header: 'Unit', width: '7%', render: (f) => f.unit ?? '' },
  {
    key: 'source',
    header: 'Filled by',
    width: '20%',
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
  { key: 'desc', header: 'Notes', render: (f) => f.description ?? (f.enum_values ? f.enum_values.join(' | ') : '') },
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

  const fieldsTitle = editing
    ? `Draft version ${draft?.version ?? (latestPublished ? latestPublished.version + 1 : 1)}`
    : current
      ? `Version ${current.version} fields`
      : 'Fields'

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
            empty="LOADING…"
          />
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel title={fieldsTitle} badge={current?.notes && !editing ? current.notes : undefined} persistKey="ot.panel.schemaFields">
        <div className="panel-body">
          {editing && (
            <div className="toolbar">
              <span className="muted">
                Each field: <span className="mono">key</span>, <span className="mono">type</span> (string, integer,
                number, boolean, enum, timestamp, position, json) and <span className="mono">description</span> (notes);
                optionally unit, enum_values, default, required, and <span className="mono">builtin</span> to fill it
                from OpenTrack.
              </span>
              <span className="spacer" />
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
            </div>
          )}
          {error && <div className="error-text">{error}</div>}
          {editing ? (
            <>
              <div className="counts">
                {(overview?.builtins ?? []).map((b) => (
                  <Badge key={b.name} color="grey" size="sm" title={`field type must be ${b.type}`}>
                    {b.name} · {b.type}
                  </Badge>
                ))}
              </div>
              <CodeEditor aria-label="Draft fields" value={text} onChange={setText} minHeight={240} maxHeight={560} />
            </>
          ) : (
            <DataTable
              aria-label="Output schema fields"
              columns={FIELD_COLUMNS}
              rows={current?.fields ?? []}
              rowKey={(f) => f.key}
              empty={current?.version === 1 ? 'Version 1 has no fields: tracks publish only the always-published set.' : 'No fields.'}
            />
          )}
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel title="Always published" persistKey="ot.panel.schemaCore">
        <div className="panel-body">
          <span className="muted">
            The OTH-GOLD minimum (contact and position sets) and a symbol code (SIDC) are in every track message. The
            output schema adds <span className="mono">attributes</span>: each field is filled from the entity's card,
            else from a feed mapping (<span className="mono">ext.&lt;field&gt;</span> in the mapping studio), or linked
            to an OpenTrack value.
          </span>
          <div className="counts">
            {(overview?.published_core ?? []).map((f) => (
              <Badge key={f} color="grey" size="sm">
                {f}
              </Badge>
            ))}
          </div>
        </div>
      </CollapsiblePanel>
    </div>
  )
}
