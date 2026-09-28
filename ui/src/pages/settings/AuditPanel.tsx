import { useCallback, useEffect, useState } from 'react'
import { TbDownload, TbSearch, TbShieldCheck } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, FieldSelect, Input, useToast, type DataTableColumn } from 'staresdk'
import { api, type AuditQuery, type AuditRow, type AuditVerify } from '../../api/client'
import { InfoTip } from '../../components/InfoTip'
import { errorMessage, fmtTime } from '../../lib/format'
import { INPUT } from '../../lib/valueSpec'
import { SettingsRow } from './SettingsRow'

const PAGE = 200
const OUTCOMES = [{ name: 'success' }, { name: 'failure' }]

/** A `datetime-local` value (local time) as Unix ms; empty or invalid: none. */
const toMs = (v: string) => {
  const t = v ? new Date(v).getTime() : NaN
  return Number.isFinite(t) ? t : undefined
}

/** A row's details in a line: the decision's reason and evidence, or the event's fields. */
function summary(r: AuditRow): string {
  const d = r.detail ?? {}
  const text = JSON.stringify(d)
  return text === '{}' ? '' : text
}

/**
 * Settings → Audit: the hash-chained audit record (admins). Every decision and every sign-in event,
 * filtered by time, account and operation, exported as CSV, and the chain checked end to end.
 */
export function AuditPanel() {
  const { toast } = useToast()
  const [from, setFrom] = useState('')
  const [to, setTo] = useState('')
  const [actor, setActor] = useState('')
  const [op, setOp] = useState('')
  const [outcome, setOutcome] = useState<string | null>(null)
  const [rows, setRows] = useState<AuditRow[] | null>(null)
  const [next, setNext] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [verify, setVerify] = useState<AuditVerify | null>(null)
  const [verifying, setVerifying] = useState(false)

  const query = useCallback(
    (): AuditQuery => ({
      from_ms: toMs(from),
      to_ms: toMs(to),
      actor: actor.trim() || undefined,
      op: op.trim() || undefined,
      outcome: (outcome as AuditQuery['outcome']) ?? undefined,
    }),
    [from, to, actor, op, outcome],
  )

  const search = useCallback(
    async (more = false) => {
      setBusy(true)
      try {
        const r = await api.audit({ ...query(), limit: PAGE, ...(more && next != null ? { before_seq: next } : {}) })
        setRows((have) => (more ? [...(have ?? []), ...r.rows] : r.rows))
        setNext(r.next_before_seq)
      } catch (e) {
        toast({ variant: 'error', title: 'Audit record', message: errorMessage(e) })
      } finally {
        setBusy(false)
      }
    },
    [query, next, toast],
  )
  // The newest rows when the panel opens.
  useEffect(() => {
    api.audit({ limit: PAGE }).then(
      (r) => {
        setRows(r.rows)
        setNext(r.next_before_seq)
      },
      (e) => toast({ variant: 'error', title: 'Audit record', message: errorMessage(e) }),
    )
  }, [toast])

  const check = async () => {
    setVerifying(true)
    try {
      const v = await api.verifyAudit()
      setVerify(v)
      toast(
        v.ok
          ? { variant: 'success', title: 'Chain verified', message: `${v.rows.toLocaleString()} rows, each follows the one before.` }
          : { variant: 'error', title: 'Chain broken', message: `${v.problems.length} problem(s), first at row ${v.problems[0]?.seq}.` },
      )
    } catch (e) {
      toast({ variant: 'error', title: 'Not verified', message: errorMessage(e) })
    } finally {
      setVerifying(false)
    }
  }

  const columns: DataTableColumn<AuditRow>[] = [
    { key: 'seq', header: '#', width: 70, mono: true, render: (r) => r.seq, sortValue: (r) => r.seq },
    { key: 'at', header: 'Time (UTC)', width: 170, mono: true, render: (r) => fmtTime(r.at_ms), sortValue: (r) => r.at_ms },
    { key: 'actor', header: 'Actor', width: 200, render: (r) => r.actor, sortValue: (r) => r.actor },
    { key: 'op', header: 'Operation', width: 170, render: (r) => r.op, sortValue: (r) => r.op },
    {
      key: 'outcome',
      header: 'Outcome',
      width: 90,
      sortValue: (r) => r.outcome,
      render: (r) => (
        <Badge size="sm" color={r.outcome === 'failure' ? 'danger' : 'grey'}>
          {r.outcome}
        </Badge>
      ),
    },
    { key: 'ip', header: 'Address', width: 130, mono: true, render: (r) => r.ip ?? '—', sortValue: (r) => r.ip },
    {
      key: 'detail',
      header: 'Details',
      mono: true,
      render: (r) => {
        const s = summary(r)
        return (
          <span className="small" title={s} style={{ display: 'block', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {r.decision_id != null && <span className="muted">decision {r.decision_id} </span>}
            {s || '—'}
          </span>
        )
      },
    },
  ]

  return (
    <CollapsiblePanel
      title="Audit"
      persistKey="ot.panel.settings.audit"
      titleActions={
        <InfoTip label="Audit">
          Every decision (track management, configuration, accounts) and every sign-in event: signing in and out, failures with their reason and address,
          lockouts, session time-outs, password changes, accounts turned off. Rows never change; each carries a SHA-256 hash of the row before it and its
          own content, so Verify chain finds any row changed, removed or inserted. Exports and checks are recorded too.
        </InfoTip>
      }
      actions={
        <div className="num-row">
          <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.auditCsvUrl(query()), '_self')}>
            CSV
          </Button>
          <Button size="sm" variant="ghost" icon={<TbShieldCheck />} disabled={verifying} onClick={check}>
            {verifying ? 'Verifying…' : 'Verify chain'}
          </Button>
        </div>
      }
    >
      <div className="panel-body stack">
        <form
          className="stack"
          onSubmit={(e) => {
            e.preventDefault()
            search(false)
          }}
        >
          <SettingsRow label="Time" hint="Your local time; either end may be empty.">
            <div className="num-row">
              <Input style={{ ...INPUT, width: 200 }} type="datetime-local" aria-label="From" value={from} onChange={(e) => setFrom(e.target.value)} />
              <span className="muted">to</span>
              <Input style={{ ...INPUT, width: 200 }} type="datetime-local" aria-label="To" value={to} onChange={(e) => setTo(e.target.value)} />
            </div>
          </SettingsRow>
          <SettingsRow label="Actor" hint="An account's email, or engine, system, saml, cli. Exact; case does not matter.">
            <Input style={{ ...INPUT, width: 260 }} aria-label="Actor" value={actor} onChange={(e) => setActor(e.target.value)} spellCheck={false} />
          </SettingsRow>
          <SettingsRow
            label="Operations"
            hint="Comma-separated, e.g. login, logout, account_locked, session_timeout, session_end, change_password, password_expired, account_disabled, unlock_user, auth_settings, merge, undo."
          >
            <Input style={{ ...INPUT, width: 420 }} aria-label="Operations" placeholder="login, account_locked" value={op} onChange={(e) => setOp(e.target.value)} spellCheck={false} />
          </SettingsRow>
          <SettingsRow label="Outcome">
            <div className="num-row">
              <FieldSelect ariaLabel="Outcome" allowNone fields={OUTCOMES} value={outcome} onChange={setOutcome} style={{ width: 140 }} />
              <Button size="sm" type="submit" icon={<TbSearch />} disabled={busy}>
                Search
              </Button>
            </div>
          </SettingsRow>
        </form>
        {verify && (
          <div className="num-row">
            <Badge size="sm" color={verify.ok ? 'success' : 'danger'}>
              {verify.ok ? 'chain intact' : 'chain broken'}
            </Badge>
            <span className="muted small">
              rows {verify.first_seq ?? '—'}–{verify.last_seq ?? '—'}
              {verify.anchor ? ` (after a purge through ${verify.anchor.seq})` : ''}; head
            </span>
            <span className="mono small" title="Keep this elsewhere: a later check that no longer reaches it shows rows cut off the end.">
              {verify.head_hash.slice(0, 16)}…
            </span>
            {!verify.ok && <span className="small">{verify.problems.map((p) => `#${p.seq}: ${p.problem}`).join('; ')}</span>}
          </div>
        )}
        {rows === null ? (
          <span className="muted">LOADING…</span>
        ) : (
          <>
            <DataTable aria-label="Audit record" columns={columns} rows={rows} rowKey={(r) => String(r.seq)} density="compact" maxHeight={480} empty="No rows match." defaultSort={{ key: 'seq', direction: 'desc' }} />
            {next != null && (
              <div>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => search(true)}>
                  Older rows
                </Button>
              </div>
            )}
          </>
        )}
      </div>
    </CollapsiblePanel>
  )
}
