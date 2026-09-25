import { useCallback, useEffect, useMemo, useState } from 'react'
import { TbAlertTriangle } from 'react-icons/tb'
import { Badge, DataTable, FieldSelect, Input, Label, SaveButton, type DataTableColumn } from 'staresdk'
import { CodeEditor } from 'staresdk/code-editor'
import { api, type CardView, type ExtensionField } from '../../api/client'
import { DetailDrawer } from '../../lib/DetailDrawer'
import { ago, errorMessage, fmtTime, show } from '../../lib/format'

type Revision = CardView['revisions'][number]
type LinkedTrack = CardView['tracks'][number]

const TRACK_COLUMNS: DataTableColumn<LinkedTrack>[] = [
  { key: 'track', header: 'Track', mono: true, width: 150, render: (t) => t.track_id },
  { key: 'source', header: 'Source', mono: true, render: (t) => t.source_id },
  { key: 'state', header: 'State', width: 90, render: (t) => t.state },
  { key: 'last', header: 'Last', width: 60, align: 'right', render: (t) => ago(t.last_seen) },
  {
    key: 'notices',
    header: 'Differs',
    width: 70,
    align: 'right',
    render: (t) =>
      t.notices.length > 0 ? (
        <Badge color="warning" size="sm">
          {t.notices.length}
        </Badge>
      ) : (
        ''
      ),
  },
]

const REVISION_COLUMNS: DataTableColumn<Revision>[] = [
  { key: 'at', header: 'Saved', mono: true, width: 150, render: (r) => fmtTime(r.saved_at_ms) },
  { key: 'by', header: 'By', mono: true, width: 110, render: (r) => r.actor ?? '—' },
  { key: 'values', header: 'Values', mono: true, render: (r) => Object.entries(r.values).map(([k, v]) => `${k}=${show(v)}`).join(' · ') },
]

/** Editable text for a stored value. */
function toText(f: ExtensionField, v: unknown): string {
  if (v === undefined || v === null) return ''
  if (f.type === 'json') return JSON.stringify(v, null, 2)
  return typeof v === 'string' ? v : JSON.stringify(v)
}

/** A card field's draft text back to a value; the server checks and coerces it. */
function fromText(f: ExtensionField, text: string): unknown {
  const t = text.trim()
  if (t === '') return null
  if (f.type === 'boolean') return t === 'true'
  if (f.type === 'json' || f.type === 'position') {
    try {
      return JSON.parse(t)
    } catch {
      return t
    }
  }
  return t
}

function FieldInput({ field, text, onChange }: { field: ExtensionField; text: string; onChange: (t: string) => void }) {
  if (field.type === 'boolean' || field.type === 'enum') {
    const options = field.type === 'boolean' ? ['true', 'false'] : (field.enum_values ?? [])
    return (
      <FieldSelect
        ariaLabel={field.key}
        allowNone
        fields={options.map((name) => ({ name }))}
        value={text || null}
        onChange={(v) => onChange(v ?? '')}
      />
    )
  }
  if (field.type === 'json' || field.type === 'position') {
    return (
      <CodeEditor
        aria-label={field.key}
        value={text}
        onChange={onChange}
        minHeight={48}
        maxHeight={160}
        placeholder={field.type === 'position' ? '{"latitude": 32.7, "longitude": -117.2}' : 'JSON'}
      />
    )
  }
  return (
    <Input
      id={`card-${field.key}`}
      value={text}
      type={field.type === 'integer' || field.type === 'number' ? 'number' : 'text'}
      placeholder={field.type === 'timestamp' ? '2026-09-25T00:00:00Z' : undefined}
      onChange={(e) => onChange(e.target.value)}
      autoComplete="off"
      spellCheck={false}
    />
  )
}

/**
 * The card form in a right drawer. With `entityId` it edits that entity's card; without one it
 * starts a card for track `fromTrack` (registering the track's identifiers as a new entity), and
 * nothing is created until the user saves.
 */
export function CardEditor({
  entityId,
  fromTrack,
  title,
  open,
  onClose,
  onSaved,
}: {
  entityId?: string | null
  fromTrack: string
  title: string
  open: boolean
  onClose: () => void
  onSaved: (entityId: string) => void
}) {
  const [view, setView] = useState<CardView | null>(null)
  const [fields, setFields] = useState<ExtensionField[] | null>(null)
  const [draft, setDraft] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)

  const adopt = useCallback((v: CardView) => {
    setView(v)
    setFields(v.schema.fields)
    setDraft(Object.fromEntries(v.schema.fields.map((f) => [f.key, toText(f, v.card?.values[f.key])])))
  }, [])

  // Keyed by track and entity in the page, so a change of either remounts with a clean state.
  useEffect(() => {
    if (entityId) {
      api.card(entityId).then(adopt, (e) => setError(errorMessage(e)))
    } else {
      // A new card follows the latest published output schema.
      api.schema().then(
        (o) => {
          const latest = o.versions.find((v) => v.version === o.latest_published)
          setView(null)
          setFields(latest?.fields ?? [])
          setDraft({})
        },
        (e) => setError(errorMessage(e)),
      )
    }
  }, [entityId, adopt])

  const editable = useMemo(() => (fields ?? []).filter((f) => !f.builtin), [fields])
  const dirty = useMemo(
    () => editable.some((f) => (draft[f.key] ?? '') !== toText(f, view?.card?.values[f.key])),
    [view, editable, draft],
  )
  // What each live track's feed reports for a field, where it differs from the card.
  const differences = useMemo(() => {
    const out: Record<string, { source: string; feed: unknown }[]> = {}
    for (const t of view?.tracks ?? []) {
      for (const n of t.notices) (out[n.key] ??= []).push({ source: `${t.source_id} (${t.track_id})`, feed: n.feed })
    }
    return out
  }, [view])

  const save = async () => {
    setSaving(true)
    setError(null)
    try {
      const values = Object.fromEntries(editable.map((f) => [f.key, fromText(f, draft[f.key] ?? '')]))
      const v = entityId ? await api.saveCard(entityId, values) : await api.createCard({ from_track: fromTrack, values })
      adopt(v)
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
      onSaved(v.entity.id)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setSaving(false)
    }
  }

  const builtins = (fields ?? []).filter((f) => f.builtin)
  return (
    <DetailDrawer
      open={open}
      onClose={onClose}
      label="Card editor"
      storageKey="ot.cardDetail.width"
      title={view?.entity.name ?? title}
      status={
        entityId ? undefined : (
          <Badge color="grey" size="sm" uppercase>
            new card
          </Badge>
        )
      }
      actions={fields && <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />}
    >
      <div className="panel-body">
        {view && (
          <div className="counts">
            <span className="muted mono">{view.entity.id}</span>
            {view.entity.identifiers.map((i) => (
              <Badge key={`${i.scheme}:${i.value}`} color="grey" size="sm">
                {i.scheme}:{i.value}
              </Badge>
            ))}
          </div>
        )}
        {!entityId && (
          <span className="muted">
            Saving registers this track's identifiers as a new entity with these values. Card values are published in
            the track's attributes and take precedence over what feeds report.
          </span>
        )}
        {error && <div className="error-text">{error}</div>}
        {!fields ? (
          !error && <span className="muted">LOADING…</span>
        ) : editable.length === 0 ? (
          <p className="muted">The output schema has no fields a card can fill. Add fields in the Schema workspace.</p>
        ) : (
          <div className="card-fields">
            {editable.map((f) => (
              <div key={f.key} className="field">
                <Label htmlFor={`card-${f.key}`} size="sm">
                  {f.key}
                  {f.unit ? ` (${f.unit})` : ''}
                </Label>
                <FieldInput field={f} text={draft[f.key] ?? ''} onChange={(t) => setDraft({ ...draft, [f.key]: t })} />
                {f.description && <span className="muted">{f.description}</span>}
                {(differences[f.key] ?? []).map((d) => (
                  <span key={d.source} className="notice">
                    <TbAlertTriangle aria-hidden /> {d.source} reports <span className="mono">{show(d.feed)}</span>; the card
                    value is published.
                  </span>
                ))}
              </div>
            ))}
          </div>
        )}
        {builtins.length > 0 && (
          <p className="muted" style={{ margin: 0 }}>
            Filled by OpenTrack, not the card: {builtins.map((f) => f.key).join(', ')}.
          </p>
        )}
        {view && (
          <>
            <h3 className="subhead">Live tracks using this card</h3>
            <DataTable
              aria-label="Live tracks using this card"
              columns={TRACK_COLUMNS}
              rows={view.tracks}
              rowKey={(t) => t.uid}
              maxHeight={200}
              empty="No live track resolves to this entity right now."
            />
            <h3 className="subhead">History</h3>
            <DataTable
              aria-label="Card history"
              columns={REVISION_COLUMNS}
              rows={view.revisions}
              rowKey={(r) => String(r.id)}
              maxHeight={200}
              empty="Never edited."
            />
          </>
        )}
      </div>
    </DetailDrawer>
  )
}
