import { useEffect, useState } from 'react'
import { Badge } from 'staresdk'
import { api, type DependencyStatus, type ServerStatus } from '../api/client'

const REFRESH_MS = 5000

function Dependency({ name, dep, detail }: { name: string; dep?: DependencyStatus; detail: string }) {
  return (
    <div className="dep">
      <span className="name">{name}</span>
      {dep === undefined ? (
        <Badge color="grey" uppercase>checking</Badge>
      ) : dep.ok ? (
        <Badge color="success" uppercase>up</Badge>
      ) : (
        <Badge color="danger" uppercase>down</Badge>
      )}
      <span className="mono muted">{dep && !dep.ok ? dep.error : detail}</span>
    </div>
  )
}

/** Health of every dependency the server needs, refreshed every few seconds. */
export function StatusPanel({ onStatus }: { onStatus?: (s: ServerStatus | null) => void }) {
  const [status, setStatus] = useState<ServerStatus | null>(null)
  const [unreachable, setUnreachable] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    const load = () =>
      api
        .status()
        .then((s) => {
          if (cancelled) return
          setStatus(s)
          setUnreachable(null)
          onStatus?.(s)
        })
        .catch((e: Error) => {
          if (cancelled) return
          setUnreachable(e.message)
          onStatus?.(null)
        })
    load()
    const timer = setInterval(load, REFRESH_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [onStatus])

  return (
    <section className="section" aria-labelledby="status-heading">
      <h2 id="status-heading">System status</h2>
      <Dependency
        name="Control plane"
        dep={unreachable ? { ok: false, error: unreachable } : status ? { ok: true } : undefined}
        detail={status ? `v${status.version} · ${status.node_id}` : ''}
      />
      <Dependency
        name="SQLite"
        dep={status?.sqlite}
        detail={status ? `schema v${status.sqlite.schema_version} · ${status.sqlite.path}` : ''}
      />
      <Dependency
        name="Redis"
        dep={status?.redis}
        detail={status ? `namespace ${status.redis.namespace}` : ''}
      />
      <Dependency
        name="NATS"
        dep={status?.nats}
        detail={
          status?.nats.ok
            ? `${status.nats.server_name} · stream ${status.nats.stream} (${status.nats.stream_messages ?? 0} msgs) · ${status.nats.tracks_subject}.>`
            : ''
        }
      />
    </section>
  )
}
