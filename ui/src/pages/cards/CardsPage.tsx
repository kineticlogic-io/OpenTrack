import { useCallback, useEffect, useMemo, useState } from 'react'
import { TbAlertTriangle, TbSearch } from 'react-icons/tb'
import { Badge, DataTable, FieldSelect, Input, Label, SaveButton, type DataTableColumn } from 'staresdk'
import { CodeEditor } from 'staresdk/code-editor'
import { api, type CardView, type Entity, type ExtensionField } from '../../api/client'
import { ago, errorMessage, fmtTime } from '../../lib/format'

type Hit = Entity & { has_card: boolean }
type Revision = CardView['revisions'][number]
type LinkedTrack = CardView['tracks'][number]

const idents = (e: Entity) => e.identifiers.map((i) => `${i.scheme}:${i.value}`).join(' · ')

const HIT_COLUMNS: DataTableColumn<Hit>[] = [
  { key: 'name', header: 'Entity', render: (e) => e.name ?? <span className="muted">{e.id}</span>, sortValue: (e) => e.name ?? e.id },
  { key: 'ids', header: 'Identifiers', mono: true, render: idents },
  {
    key: 'card',
    header: 'Card',
    width: 64,
    render: (e) =>
      e.has_card ? (
        <Badge color="blue" size="sm">
          card
        </Badge>
      ) : (
        ''
      ),
  },
]

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

const show = (v: unknown) => (typeof v === 'string' ? v : JSON.stringify(v))

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
  const id = `card-${field.key}`
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
      id={id}
      value={text}
      type={field.type === 'integer' || field.type === 'number' ? 'number' : 'text'}
      placeholder={field.type === 'timestamp' ? '2026-09-25T00:00:00Z' : undefined}
      onChange={(e) => onChange(e.target.value)}
      autoComplete="off"
      spellCheck={false}
    />
  )
}

function CardEditor({ id }: { id: string }) {
  const [view, setView] = useState<CardView | null>(null)
  const [draft, setDraft] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)

  const adopt = useCallback((v: CardView) => {
    setView(v)
    setDraft(Object.fromEntries(v.schema.fields.map((f) => [f.key, toText(f, v.card?.values[f.key])])))
  }, [])

  useEffect(() => {
    api.card(id).then(adopt, (e) => setError(errorMessage(e)))
  }, [id, adopt])

  const editable = useMemo(() => (view?.schema.fields ?? []).filter((f) => !f.builtin), [view])
  const dirty = useMemo(
    () => !!view && editable.some((f) => draft[f.key] !== toText(f, view.card?.values[f.key])),
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
    if (!view) return
    setSaving(true)
    setError(null)
    try {
      const values = Object.fromEntries(editable.map((f) => [f.key, fromText(f, draft[f.key] ?? '')]))
      adopt(await api.saveCard(id, values))
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setSaving(false)
    }
  }

  if (!view) {
    return <section className="section">{error ? <div className="error-text">{error}</div> : <span className="muted">Loading…</span>}</section>
  }

  const revisionColumns: DataTableColumn<Revision>[] = [
    { key: 'at', header: 'Saved', mono: true, width: 150, render: (r) => fmtTime(r.saved_at_ms) },
    { key: 'by', header: 'By', mono: true, width: 110, render: (r) => r.actor ?? '—' },
    { key: 'values', header: 'Values', mono: true, render: (r) => Object.entries(r.values).map(([k, v]) => `${k}=${show(v)}`).join(' · ') },
  ]
  const linkedBuiltins = view.schema.fields.filter((f) => f.builtin)

  return (
    <section className="section" aria-labelledby="card-heading">
      <div className="section-head">
        <h2 id="card-heading">{view.entity.name ?? view.entity.id}</h2>
        <span className="muted mono">{view.entity.id}</span>
        <span className="spacer" />
        <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />
      </div>
      <div className="counts" style={{ marginBottom: 10 }}>
        {view.entity.identifiers.map((i) => (
          <Badge key={`${i.scheme}:${i.value}`} color="grey" size="sm">
            {i.scheme}:{i.value}
          </Badge>
        ))}
      </div>
      {error && (
        <div className="error-text" style={{ marginBottom: 8 }}>
          {error}
        </div>
      )}
      {editable.length === 0 ? (
        <p className="muted">
          Output schema version {view.schema.version} has no fields a card can fill. Add fields in the Schema workspace.
        </p>
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
      {linkedBuiltins.length > 0 && (
        <p className="muted" style={{ marginTop: 8 }}>
          Filled by OpenTrack, not the card: {linkedBuiltins.map((f) => f.key).join(', ')}.
        </p>
      )}
      <h3>Live tracks using this card</h3>
      <DataTable
        aria-label="Live tracks using this card"
        columns={TRACK_COLUMNS}
        rows={view.tracks}
        rowKey={(t) => t.uid}
        maxHeight={200}
        empty="No live track resolves to this entity right now."
      />
      <h3>History</h3>
      <DataTable
        aria-label="Card history"
        columns={revisionColumns}
        rows={view.revisions}
        rowKey={(r) => String(r.id)}
        maxHeight={200}
        empty="Never edited."
      />
    </section>
  )
}

/** Find an entity and fill in its baseball card. */
export default function CardsPage({ selected, onSelect }: { selected: string; onSelect: (id: string) => void }) {
  const [query, setQuery] = useState('')
  const [hits, setHits] = useState<Hit[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const q = query.trim()
    if (q.length < 2) return
    // Search as the user types, after a short pause.
    const t = setTimeout(() => {
      api.searchCards(q).then(
        (h) => {
          setHits(h)
          setError(null)
        },
        (e) => setError(errorMessage(e)),
      )
    }, 250)
    return () => clearTimeout(t)
  }, [query])

  return (
    <div className="split">
      <section className="section" aria-labelledby="cards-heading">
        <div className="section-head">
          <h2 id="cards-heading">Cards</h2>
        </div>
        <div className="field" style={{ marginBottom: 8 }}>
          <Label htmlFor="card-search" size="sm">
            Name or identifier
          </Label>
          <Input
            id="card-search"
            value={query}
            placeholder="e.g. TRUMAN, 338924210, NL504"
            onChange={(e) => setQuery(e.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        {error && <div className="error-text">{error}</div>}
        <DataTable
          aria-label="Entities"
          columns={HIT_COLUMNS}
          rows={query.trim().length < 2 ? [] : (hits ?? [])}
          rowKey={(e) => e.id}
          selectedKey={selected || null}
          onRowClick={(e) => onSelect(e.id)}
          empty={
            query.trim().length < 2 ? (
              <span className="muted">
                <TbSearch aria-hidden /> Search for a ship, aircraft or emitter. To start a card for a live track, look it up
                on the Overview.
              </span>
            ) : (
              'No matching entity.'
            )
          }
        />
      </section>
      {selected ? (
        <CardEditor key={selected} id={selected} />
      ) : (
        <section className="section">
          <span className="muted">
            Select an entity to fill in its card. Card values are published in a track's attributes and take
            precedence over what feeds report.
          </span>
        </section>
      )}
    </div>
  )
}
