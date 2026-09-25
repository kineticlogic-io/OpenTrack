import { lazy, Suspense, useState, type FormEvent } from 'react'
import { TbAlertTriangle, TbId, TbSearch } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, Input, Label, useToast, type BadgeColor } from 'staresdk'
import { api, ApiError, type GraphEdge, type TrackResponse } from '../api/client'
import { errorMessage } from '../lib/format'

// React Flow loads only when a lineage is shown.
const LineageGraph = lazy(() => import('./LineageGraph'))

const STATE_COLOR: Record<string, BadgeColor> = {
  tentative: 'warning',
  confirmed: 'success',
  lost: 'grey',
  dropped: 'danger',
}

const fmtTime = (ms: number | string) => new Date(ms).toISOString().replace('T', ' ').slice(0, 19) + 'Z'
const fmtNum = (v: number | undefined, digits: number, unit = '') =>
  v === undefined ? '—' : `${v.toFixed(digits)}${unit}`

/** Look up a system track by UID and show its state and graph history. */
export function TrackLookup() {
  const { toast } = useToast()
  const [query, setQuery] = useState('')
  const [loading, setLoading] = useState(false)
  const [result, setResult] = useState<{ track: TrackResponse | null; edges: GraphEdge[] } | null>(null)

  const lookup = async (e: FormEvent) => {
    e.preventDefault()
    const uid = query.trim()
    if (!uid) return
    setLoading(true)
    try {
      const [track, explain] = await Promise.allSettled([api.track(uid), api.explain(uid)])
      const edges = explain.status === 'fulfilled' ? explain.value.edges : []
      const live = track.status === 'fulfilled' ? track.value : null
      if (!live && edges.length === 0) {
        const err = track.status === 'rejected' ? (track.reason as Error) : null
        toast({
          variant: err instanceof ApiError && err.status === 400 ? 'warning' : 'info',
          message: err?.message ?? `No system track ${uid}`,
        })
        setResult(null)
      } else {
        setResult({ track: live, edges })
      }
    } finally {
      setLoading(false)
    }
  }

  const openCard = (id: string) => {
    window.location.hash = `cards/${encodeURIComponent(id)}`
  }
  const createCard = async (uid: string) => {
    try {
      const v = await api.createCard({ from_track: uid })
      toast({ variant: 'success', message: `Card started for ${v.entity.name ?? v.entity.id}.` })
      openCard(v.entity.id)
    } catch (e) {
      toast({ variant: 'error', title: 'Could not start a card', message: errorMessage(e) })
    }
  }

  const t = result?.track?.track
  const m = result?.track?.message
  const show = (v: unknown) => (typeof v === 'string' ? v : JSON.stringify(v))
  return (
    <CollapsiblePanel title="System track lookup" persistKey="ot.panel.lookup">
      <div className="panel-body">
      <form className="lookup" onSubmit={lookup}>
        <div className="field">
          <Label htmlFor="uid">UID or track id</Label>
          <Input
            id="uid"
            placeholder="OTK000000001 or tms-OTK000000001"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        <Button type="submit" icon={<TbSearch />} disabled={loading || !query.trim()}>
          Look up
        </Button>
      </form>

      {result && !t && <p className="muted">No live state (retired or not yet published); graph history below.</p>}

      {t && (
        <dl className="facts">
          <dt>Subject</dt>
          <dd className="mono">{result?.track?.subject ?? `tms-${t.uid}`}</dd>
          <dt>State</dt>
          <dd>
            <Badge color={STATE_COLOR[t.state] ?? 'grey'} uppercase>
              {t.state}
            </Badge>
          </dd>
          <dt>Class-name</dt>
          <dd>
            {m?.class}-{m?.name}
          </dd>
          <dt>Force code</dt>
          <dd className="mono">
            {String(m?.force_code).padStart(2, '0')} · {m?.domain} {m?.affiliation}
          </dd>
          <dt>Track type</dt>
          <dd className="mono">{m?.track_type}</dd>
          <dt>SIDC</dt>
          <dd className="mono">
            {m?.sidc.code} <span className="muted">({m?.sidc.standard === 'cot' ? 'CoT' : `MIL-STD-${m?.sidc.standard.toUpperCase()}`})</span>
          </dd>
          <dt>Position</dt>
          <dd className="mono">
            {t.view.position.latitude.toFixed(5)}, {t.view.position.longitude.toFixed(5)}
          </dd>
          <dt>Course / speed</dt>
          <dd className="mono">
            {fmtNum(t.view.kinematics.course_deg, 0, '°')} / {fmtNum(t.view.kinematics.speed_mps, 1, ' m/s')}
          </dd>
          <dt>Last seen</dt>
          <dd className="mono">{fmtTime(t.last_seen)}</dd>
          <dt>Observations</dt>
          <dd className="mono">{t.observation_count}</dd>
          <dt>Contributors</dt>
          <dd className="mono">
            {t.contributors.map((c) => `${c.source_id}/${c.source_track_key} (${c.pairing})`).join(', ')}
          </dd>
          <dt>Card</dt>
          <dd>
            {t.entity_id ? (
              <Button size="sm" variant="secondary" icon={<TbId />} onClick={() => openCard(t.entity_id!)}>
                Open card
              </Button>
            ) : (
              <Button size="sm" variant="secondary" icon={<TbId />} onClick={() => createCard(t.uid)}>
                Create card
              </Button>
            )}
          </dd>
        </dl>
      )}

      {t && (
        <>
          <h3 className="subhead">Published attributes</h3>
          {Object.keys(t.attributes ?? {}).length === 0 ? (
            <p className="muted">None: the output schema has no field with a value for this track.</p>
          ) : (
            <dl className="facts">
              {Object.entries(t.attributes ?? {}).map(([k, v]) => (
                <div key={k} style={{ display: 'contents' }}>
                  <dt className="mono">{k}</dt>
                  <dd className="mono">{show(v)}</dd>
                </div>
              ))}
            </dl>
          )}
          {(t.notices ?? []).map((n) => (
            <div key={n.key} className="notice">
              <TbAlertTriangle aria-hidden /> <span className="mono">{n.key}</span>: card says{' '}
              <span className="mono">{show(n.card)}</span>, {n.source_id} reports <span className="mono">{show(n.feed)}</span>.
              The card value is published.
            </div>
          ))}
        </>
      )}

      {result && result.edges.length > 0 && (
        <>
          <h3 className="subhead">Lineage</h3>
          <Suspense fallback={<span className="muted">LOADING…</span>}>
            <LineageGraph uid={t?.uid ?? query.trim().replace(/^tms-/, '')} edges={result.edges} entityId={t?.entity_id} />
          </Suspense>
          <h3 className="subhead">Decisions</h3>
          <ul className="history">
            {result.edges.map((e) => (
              <li key={e.id} className={e.valid_to_ms === null ? '' : 'ended'}>
                <Badge color={e.valid_to_ms === null ? 'blue' : 'grey'} size="sm">
                  {e.kind}
                </Badge>
                <span className="mono">
                  {e.src_key} → {e.dst_key}
                </span>
                <span />
                <span className="muted">
                  {fmtTime(e.valid_from_ms)}
                  {e.valid_to_ms !== null && ` – ${fmtTime(e.valid_to_ms)}`} · decision #{e.decision_id} ({e.decision_op} by{' '}
                  {e.decision_actor})
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
      </div>
    </CollapsiblePanel>
  )
}
