import { useState, type FormEvent } from 'react'
import { TbSearch } from 'react-icons/tb'
import { Badge, Button, Input, Label, useToast, type BadgeColor } from 'staresdk'
import { api, ApiError, type GraphEdge, type TrackResponse } from '../api/client'

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

  const t = result?.track?.track
  return (
    <section className="section" aria-labelledby="lookup-heading">
      <h2 id="lookup-heading">System track lookup</h2>
      <form className="lookup" onSubmit={lookup}>
        <div className="field">
          <Label htmlFor="uid">UID or document id</Label>
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
          <dt>Document id</dt>
          <dd className="mono">tms-{t.uid}</dd>
          <dt>State</dt>
          <dd>
            <Badge color={STATE_COLOR[t.state] ?? 'grey'} uppercase>
              {t.state}
            </Badge>
          </dd>
          <dt>Name</dt>
          <dd>{t.view.name ?? t.view.callsign ?? '—'}</dd>
          <dt>Classification</dt>
          <dd className="mono">{String(result?.track?.document.classification ?? '—')}</dd>
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
        </dl>
      )}

      {result && result.edges.length > 0 && (
        <>
          <h2>Graph history</h2>
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
    </section>
  )
}
