import { useCallback, useEffect, useState } from 'react'
import { TbCheck, TbRestore, TbX } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, FieldSelect, SaveButton, useToast, type DataTableColumn } from 'staresdk'
import { api, type CorrelationSettings, type DecisionRow, type Suggestion, type SuggestionTrack } from '../../api/client'
import { errorMessage, fmtTime } from '../../lib/format'
import { SettingsForm } from './SettingsForm'

const REFRESH_MS = 5000
const STATUSES = [{ name: 'open' }, { name: 'accepted' }, { name: 'rejected' }, { name: 'expired' }, { name: 'all' }]

function TrackLink({ t }: { t: SuggestionTrack | null }) {
  if (!t) return null
  const label = t.name ?? t.track_id
  return t.live ? (
    <a href={`#tracks/${encodeURIComponent(t.uid)}`} title={`${t.track_id}${t.published === false ? ' (not published)' : ''}`}>
      {label}
    </a>
  ) : (
    <span className="muted" title="No longer live">
      {label}
    </span>
  )
}

const OP_COLOR: Record<string, 'blue' | 'success' | 'warning' | 'danger' | 'grey'> = {
  pair: 'success',
  merge: 'success',
  split: 'warning',
  do_not_pair: 'danger',
  reject_split: 'grey',
  correlation_settings: 'blue',
}

/**
 * Correlation: the engine's pairing and split suggestions for an operator to accept or reject,
 * the settings it correlates with, and the decisions it and operators have made.
 */
export default function CorrelationPage() {
  const { toast } = useToast()
  const [status, setStatus] = useState('open')
  const [suggestions, setSuggestions] = useState<Suggestion[] | null>(null)
  const [busy, setBusy] = useState<number | null>(null)
  const [settings, setSettings] = useState<CorrelationSettings | null>(null)
  const [draft, setDraft] = useState<CorrelationSettings | null>(null)
  const [defaults, setDefaults] = useState<CorrelationSettings | null>(null)
  const [version, setVersion] = useState('')
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)
  const [decisions, setDecisions] = useState<DecisionRow[] | null>(null)
  const [picked, setPicked] = useState<number | null>(null)

  const loadSuggestions = useCallback(
    () => api.suggestions(status).then((r) => setSuggestions(r.suggestions), (e) => toast({ variant: 'error', title: 'Suggestions', message: errorMessage(e) })),
    [status, toast],
  )
  const loadDecisions = useCallback(() => api.correlationDecisions(200).then((r) => setDecisions(r.decisions), () => setDecisions([])), [])

  useEffect(() => {
    loadSuggestions()
    loadDecisions()
    const t = setInterval(() => {
      loadSuggestions()
      loadDecisions()
    }, REFRESH_MS)
    return () => clearInterval(t)
  }, [loadSuggestions, loadDecisions])

  useEffect(() => {
    api.correlationSettings().then(
      (r) => {
        setSettings(r.settings)
        setDraft(r.settings)
        setDefaults(r.defaults)
        setVersion(r.version)
      },
      (e) => toast({ variant: 'error', title: 'Correlation settings', message: errorMessage(e) }),
    )
  }, [toast])

  const decide = async (s: Suggestion, decision: 'accept' | 'reject') => {
    setBusy(s.id)
    try {
      await api.decideSuggestion(s.id, decision)
      toast({ variant: 'success', title: decision === 'accept' ? 'Accepted' : 'Rejected', message: `Suggestion ${s.id}` })
    } catch (e) {
      toast({ variant: 'error', title: `Could not ${decision}`, message: errorMessage(e) })
    } finally {
      setBusy(null)
      loadSuggestions()
      loadDecisions()
    }
  }

  const save = async () => {
    if (!draft) return
    setSaving(true)
    try {
      const r = await api.saveCorrelationSettings(draft)
      setSettings(r.settings)
      setDraft(r.settings)
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
    } catch (e) {
      toast({ variant: 'error', title: 'Settings not saved', message: errorMessage(e) })
    } finally {
      setSaving(false)
      loadDecisions()
    }
  }

  const columns: DataTableColumn<Suggestion>[] = [
    {
      key: 'kind',
      header: 'Proposal',
      width: 80,
      render: (s) => (
        <Badge color={s.kind === 'pair' ? 'blue' : 'warning'} size="sm" uppercase>
          {s.kind}
        </Badge>
      ),
      sortValue: (s) => s.kind,
    },
    {
      key: 'what',
      header: 'Tracks',
      render: (s) =>
        s.kind === 'pair' ? (
          <span>
            <TrackLink t={s.b} /> <span className="muted">into</span> <TrackLink t={s.a} />
          </span>
        ) : (
          <span>
            <span className="mono">{s.source_track}</span> <span className="muted">off</span> <TrackLink t={s.a} />
          </span>
        ),
    },
    {
      key: 'why',
      header: 'Evidence',
      render: (s) => (
        <span className="muted" title={s.evidence.reason}>
          {s.evidence.reason ?? '—'}
        </span>
      ),
    },
    { key: 'updated', header: 'Updated', width: 170, mono: true, render: (s) => fmtTime(s.updated_at_ms), sortValue: (s) => s.updated_at_ms },
    {
      key: 'act',
      header: '',
      width: 190,
      align: 'right',
      render: (s) =>
        s.status === 'open' ? (
          <span className="value-row" style={{ justifyContent: 'flex-end', flexWrap: 'nowrap' }}>
            <Button size="sm" variant="primary" icon={<TbCheck />} disabled={busy !== null} onClick={() => decide(s, 'accept')}>
              Accept
            </Button>
            <Button size="sm" variant="secondary" icon={<TbX />} disabled={busy !== null} onClick={() => decide(s, 'reject')}>
              Reject
            </Button>
          </span>
        ) : (
          <Badge color="grey" size="sm" uppercase>
            {s.status}
          </Badge>
        ),
    },
  ]

  const decisionColumns: DataTableColumn<DecisionRow>[] = [
    { key: 'at', header: 'Time', width: 170, mono: true, render: (d) => fmtTime(d.at_ms), sortValue: (d) => d.at_ms },
    {
      key: 'op',
      header: 'Decision',
      width: 170,
      render: (d) => (
        <Badge color={OP_COLOR[d.op] ?? 'grey'} size="sm">
          {d.op}
        </Badge>
      ),
      sortValue: (d) => d.op,
    },
    { key: 'actor', header: 'By', width: 110, render: (d) => d.actor, sortValue: (d) => d.actor },
    { key: 'reason', header: 'Reason', render: (d) => <span title={d.reason ?? ''}>{d.reason ?? '—'}</span> },
  ]
  const pickedDecision = decisions?.find((d) => d.id === picked) ?? null
  const dirty = draft !== null && JSON.stringify(draft) !== JSON.stringify(settings)
  const open = suggestions?.filter((s) => s.status === 'open').length ?? 0

  return (
    <div className="panels tight">
      <div className="correlation-top">
        <CollapsiblePanel
          title="Suggestions"
          badge={status === 'open' && open ? String(open) : undefined}
          persistKey="ot.panel.suggestions"
          titleActions={<FieldSelect ariaLabel="Suggestion status" fields={STATUSES} value={status} onChange={(v) => setStatus(v ?? 'open')} style={{ width: 120 }} />}
        >
          <div className="panel-body">
            <DataTable
              aria-label="Suggestions"
              columns={columns}
              rows={suggestions ?? []}
              rowKey={(s) => String(s.id)}
              maxHeight={420}
              empty={
                suggestions === null
                  ? 'LOADING…'
                  : status === 'open'
                    ? 'Nothing to decide. Pairings wait here in suggest mode, and splits when a source track stops agreeing with its track.'
                    : `No ${status === 'all' ? '' : status + ' '}suggestions.`
              }
            />
          </div>
        </CollapsiblePanel>
        <CollapsiblePanel
          title="Settings"
          persistKey="ot.panel.correlationSettings"
          titleActions={version ? <span className="mono muted">{version}</span> : undefined}
          actions={
            <>
              <Button
                size="sm"
                variant="ghost"
                icon={<TbRestore />}
                disabled={!defaults || JSON.stringify(draft) === JSON.stringify(defaults)}
                onClick={() => defaults && setDraft(defaults)}
              >
                Defaults
              </Button>
              <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />
            </>
          }
        >
          <div className="panel-body correlation-settings">{draft ? <SettingsForm value={draft} onChange={setDraft} /> : <span className="muted">LOADING…</span>}</div>
        </CollapsiblePanel>
      </div>
      <CollapsiblePanel title="Decisions" persistKey="ot.panel.correlationDecisions">
        <div className="panel-body">
          <DataTable
            aria-label="Correlation decisions"
            columns={decisionColumns}
            rows={decisions ?? []}
            rowKey={(d) => String(d.id)}
            selectedKey={picked === null ? null : String(picked)}
            onRowClick={(d) => setPicked(d.id === picked ? null : d.id)}
            maxHeight={360}
            empty={decisions === null ? 'LOADING…' : 'No correlation decisions yet.'}
          />
          {pickedDecision && (
            <pre className="mono evidence" aria-label={`Evidence of decision ${pickedDecision.id}`}>
              {JSON.stringify(pickedDecision.evidence ?? {}, null, 2)}
            </pre>
          )}
        </div>
      </CollapsiblePanel>
    </div>
  )
}
