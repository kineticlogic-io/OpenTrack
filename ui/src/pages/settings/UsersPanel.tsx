import { useCallback, useEffect, useState } from 'react'
import { TbCopy, TbKey, TbLogout, TbPlus, TbTrash } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, FieldSelect, Input, Modal, Toggle, useToast, type DataTableColumn } from 'staresdk'
import { api, ROLES, type Account, type ApiTokenRow, type Role } from '../../api/client'
import { useAuth } from '../../auth/context'
import { InfoTip } from '../../components/InfoTip'
import { errorMessage, fmtTime } from '../../lib/format'
import { INPUT } from '../../lib/valueSpec'
import { SettingsRow } from './SettingsRow'

const ROLE_FIELDS = ROLES.map((name) => ({ name }))
const asRole = (v: string | null): Role => (ROLES as readonly string[]).includes(v ?? '') ? (v as Role) : 'viewer'

const ROLES_INFO =
  'Viewer: sees everything, changes nothing. Track manager: also pairs, merges, groups and deletes tracks, edits the registry and ' +
  'accepts suggestions. Admin: also changes sources, the schema, settings and accounts.'

function ModalButtons({ onClose, children }: { onClose: () => void; children: React.ReactNode }) {
  return (
    <div className="num-row" style={{ justifyContent: 'flex-end' }}>
      <Button size="sm" variant="ghost" type="button" onClick={onClose}>
        Cancel
      </Button>
      {children}
    </div>
  )
}

function AddAccount({ onClose, onAdded }: { onClose: () => void; onAdded: () => void }) {
  const { toast } = useToast()
  const [email, setEmail] = useState('')
  const [name, setName] = useState('')
  const [role, setRole] = useState<Role>('viewer')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const add = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    try {
      const u = await api.createUser({ email: email.trim(), name: name.trim(), role, ...(password ? { password } : {}) })
      toast({ variant: 'success', title: 'Account added', message: u.email })
      onAdded()
    } catch (err) {
      toast({ variant: 'error', title: 'Account not added', message: errorMessage(err) })
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title="Add account" onClose={onClose} width={520} resizable={false}>
      <form className="panel-body stack" onSubmit={add}>
        <SettingsRow label="Email">
          <Input style={{ ...INPUT, width: 280 }} type="email" aria-label="Email" autoFocus required value={email} onChange={(e) => setEmail(e.target.value)} />
        </SettingsRow>
        <SettingsRow label="Name">
          <Input style={{ ...INPUT, width: 280 }} aria-label="Name" value={name} onChange={(e) => setName(e.target.value)} />
        </SettingsRow>
        <SettingsRow label="Role" hint={ROLES_INFO}>
          <FieldSelect ariaLabel="Role" fields={ROLE_FIELDS} value={role} onChange={(v) => setRole(asRole(v))} style={{ width: 160 }} />
        </SettingsRow>
        <SettingsRow label="Password" hint="At least 8 characters. Leave it empty for an account that signs in only with single sign-on.">
          <Input style={{ ...INPUT, width: 280 }} type="password" autoComplete="new-password" aria-label="Password" value={password} onChange={(e) => setPassword(e.target.value)} />
        </SettingsRow>
        <ModalButtons onClose={onClose}>
          <Button size="sm" type="submit" icon={<TbPlus />} disabled={busy || !email.trim()}>
            Add
          </Button>
        </ModalButtons>
      </form>
    </Modal>
  )
}

function ResetPassword({ account, onClose, onDone }: { account: Account; onClose: () => void; onDone: () => void }) {
  const { toast } = useToast()
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const run = async (next: string | null) => {
    setBusy(true)
    try {
      await api.resetPassword(account.id, next)
      toast({ variant: 'success', title: next ? 'Password set' : 'Password removed', message: `${account.email} was signed out everywhere.` })
      onDone()
    } catch (err) {
      toast({ variant: 'error', title: 'Password not changed', message: errorMessage(err) })
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title={`Password: ${account.email}`} onClose={onClose} width={480} resizable={false}>
      <form
        className="panel-body stack"
        onSubmit={(e) => {
          e.preventDefault()
          run(password)
        }}
      >
        <SettingsRow label="New password" hint="At least 8 characters. The account's sessions and API tokens end.">
          <Input style={{ ...INPUT, width: 240 }} type="password" autoComplete="new-password" aria-label="New password" autoFocus value={password} onChange={(e) => setPassword(e.target.value)} />
        </SettingsRow>
        <ModalButtons onClose={onClose}>
          {account.has_password && (
            <Button size="sm" variant="ghost" type="button" disabled={busy} onClick={() => run(null)} title="The account can then sign in only with single sign-on">
              Remove password
            </Button>
          )}
          <Button size="sm" type="submit" icon={<TbKey />} disabled={busy || !password}>
            Set password
          </Button>
        </ModalButtons>
      </form>
    </Modal>
  )
}

function NewToken({ accounts, onClose, onMade }: { accounts: Account[]; onClose: () => void; onMade: () => void }) {
  const { toast } = useToast()
  const { user } = useAuth()
  const [name, setName] = useState('')
  const [email, setEmail] = useState(user?.email ?? '')
  const [days, setDays] = useState('365')
  const [busy, setBusy] = useState(false)
  const [token, setToken] = useState<string | null>(null)
  const make = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    try {
      const account = accounts.find((a) => a.email === email)
      const r = await api.createApiToken({ name: name.trim(), ...(account ? { user_id: account.id } : {}), days: Number(days) })
      setToken(r.token)
      onMade()
    } catch (err) {
      toast({ variant: 'error', title: 'Token not made', message: errorMessage(err) })
    } finally {
      setBusy(false)
    }
  }
  const copy = () =>
    navigator.clipboard.writeText(token ?? '').then(
      () => toast({ variant: 'success', title: 'Copied', message: 'The token is on the clipboard.' }),
      () => toast({ variant: 'error', title: 'Not copied', message: 'Select the token and copy it.' }),
    )
  if (token)
    return (
      <Modal title="API token" onClose={onClose} width={560} resizable={false}>
        <div className="panel-body stack">
          <div className="num-row">
            <strong>Copy it now: it is not shown again.</strong>
            <InfoTip label="API token">Send it as the header Authorization: Bearer &lt;token&gt;. It acts as its account, with that account's role.</InfoTip>
          </div>
          <div className="token-once">{token}</div>
          <div className="num-row" style={{ justifyContent: 'flex-end' }}>
            <Button size="sm" variant="ghost" icon={<TbCopy />} onClick={copy}>
              Copy
            </Button>
            <Button size="sm" onClick={onClose}>
              Done
            </Button>
          </div>
        </div>
      </Modal>
    )
  return (
    <Modal title="New API token" onClose={onClose} width={520} resizable={false}>
      <form className="panel-body stack" onSubmit={make}>
        <SettingsRow label="Name" hint="What uses it, e.g. the script or service.">
          <Input style={{ ...INPUT, width: 260 }} aria-label="Token name" autoFocus required value={name} onChange={(e) => setName(e.target.value)} />
        </SettingsRow>
        <SettingsRow label="Acts as" hint="The account whose role it has.">
          <FieldSelect ariaLabel="Account" fields={accounts.filter((a) => a.active).map((a) => ({ name: a.email }))} value={email} onChange={(v) => setEmail(v ?? '')} style={{ width: 260 }} />
        </SettingsRow>
        <SettingsRow label="Expires in (days)">
          <Input style={{ ...INPUT, width: 100 }} type="number" min={1} max={3650} aria-label="Days" value={days} onChange={(e) => setDays(e.target.value)} />
        </SettingsRow>
        <ModalButtons onClose={onClose}>
          <Button size="sm" type="submit" icon={<TbPlus />} disabled={busy || !name.trim() || !(Number(days) > 0)}>
            Make token
          </Button>
        </ModalButtons>
      </form>
    </Modal>
  )
}

/** Settings → Users: accounts and API tokens (admins only). */
export function UsersPanel() {
  const { toast, confirm } = useToast()
  const { user: me } = useAuth()
  const [accounts, setAccounts] = useState<Account[] | null>(null)
  const [tokens, setTokens] = useState<ApiTokenRow[] | null>(null)
  const [adding, setAdding] = useState(false)
  const [resetting, setResetting] = useState<Account | null>(null)
  const [makingToken, setMakingToken] = useState(false)
  const [now, setNow] = useState(0)
  const load = useCallback(() => {
    api.users().then(setAccounts, (e) => toast({ variant: 'error', title: 'Accounts', message: errorMessage(e) }))
    api.apiTokens().then((ts) => {
      setTokens(ts)
      setNow(Date.now())
    }, (e) => toast({ variant: 'error', title: 'API tokens', message: errorMessage(e) }))
  }, [toast])
  useEffect(load, [load])

  const change = async (a: Account, patch: { role?: Role; active?: boolean }) => {
    try {
      const u = await api.updateUser(a.id, patch)
      setAccounts((xs) => (xs ?? []).map((x) => (x.id === u.id ? u : x)))
    } catch (e) {
      toast({ variant: 'error', title: 'Not changed', message: errorMessage(e) })
    }
  }
  const revoke = async (a: Account) => {
    if (!(await confirm(`Every session and API token of ${a.email} ends now.`, { title: 'Sign out everywhere', confirmLabel: 'Sign out' }))) return
    try {
      await api.revokeSessions(a.id)
      toast({ variant: 'success', title: 'Signed out', message: a.email })
      load()
    } catch (e) {
      toast({ variant: 'error', title: 'Not signed out', message: errorMessage(e) })
    }
  }
  const remove = async (a: Account) => {
    if (!(await confirm(`${a.email} is deleted with its API tokens. The decision log keeps what it did.`, { title: 'Delete account', confirmLabel: 'Delete' }))) return
    try {
      await api.deleteUser(a.id)
      load()
    } catch (e) {
      toast({ variant: 'error', title: 'Not deleted', message: errorMessage(e) })
    }
  }
  const revokeToken = async (t: ApiTokenRow) => {
    if (!(await confirm(`"${t.name}" (${t.user_email}) stops working now.`, { title: 'Revoke API token', confirmLabel: 'Revoke' }))) return
    try {
      await api.revokeApiToken(t.jti)
      load()
    } catch (e) {
      toast({ variant: 'error', title: 'Not revoked', message: errorMessage(e) })
    }
  }

  const columns: DataTableColumn<Account>[] = [
    { key: 'email', header: 'Email', render: (a) => a.email, sortValue: (a) => a.email },
    { key: 'name', header: 'Name', render: (a) => a.name || <span className="muted">—</span>, sortValue: (a) => a.name },
    {
      key: 'role',
      header: 'Role',
      width: 150,
      sortValue: (a) => ROLES.indexOf(a.role),
      render: (a) => <FieldSelect ariaLabel={`Role of ${a.email}`} fields={ROLE_FIELDS} value={a.role} onChange={(v) => v !== a.role && change(a, { role: asRole(v) })} style={{ width: 130 }} />,
    },
    {
      key: 'origin',
      header: 'Origin',
      width: 110,
      sortValue: (a) => a.origin,
      render: (a) => (
        <span className="num-row">
          {a.origin}
          {!a.has_password && (
            <Badge size="sm" color="grey" title="No password: single sign-on only">
              SSO
            </Badge>
          )}
        </span>
      ),
    },
    {
      key: 'active',
      header: 'Active',
      width: 70,
      sortValue: (a) => (a.active ? 1 : 0),
      render: (a) => <Toggle size="sm" aria-label={`${a.email} active`} value={a.active} disabled={a.id === me?.id} onChange={(active) => change(a, { active })} />,
    },
    { key: 'last', header: 'Last sign-in', width: 170, mono: true, sortValue: (a) => a.last_login_at_ms, render: (a) => fmtTime(a.last_login_at_ms) },
    {
      key: 'actions',
      header: '',
      align: 'right',
      width: 110,
      render: (a) => (
        <span className="num-row" style={{ justifyContent: 'flex-end' }}>
          <Button size="xs" variant="ghost" icon={<TbKey />} title="Set or remove password" aria-label={`Password of ${a.email}`} onClick={() => setResetting(a)} />
          <Button size="xs" variant="ghost" icon={<TbLogout />} title="Sign out everywhere" aria-label={`Sign out ${a.email} everywhere`} onClick={() => revoke(a)} />
          <Button
            size="xs"
            variant="ghost"
            icon={<TbTrash />}
            title={a.id === me?.id ? 'You cannot delete your own account' : 'Delete'}
            aria-label={`Delete ${a.email}`}
            disabled={a.id === me?.id}
            onClick={() => remove(a)}
          />
        </span>
      ),
    },
  ]

  const tokenColumns: DataTableColumn<ApiTokenRow>[] = [
    { key: 'name', header: 'Name', render: (t) => t.name, sortValue: (t) => t.name },
    { key: 'user', header: 'Acts as', render: (t) => t.user_email, sortValue: (t) => t.user_email },
    { key: 'by', header: 'Made by', render: (t) => t.created_by, sortValue: (t) => t.created_by },
    { key: 'created', header: 'Made', width: 170, mono: true, render: (t) => fmtTime(t.created_at_ms), sortValue: (t) => t.created_at_ms },
    { key: 'expires', header: 'Expires', width: 170, mono: true, render: (t) => fmtTime(t.expires_at_ms), sortValue: (t) => t.expires_at_ms },
    {
      key: 'state',
      header: 'State',
      width: 80,
      render: (t) =>
        t.revoked_at_ms ? (
          <Badge size="sm" color="grey">
            revoked
          </Badge>
        ) : t.expires_at_ms < now ? (
          <Badge size="sm" color="warning">
            expired
          </Badge>
        ) : (
          <Badge size="sm" color="success">
            active
          </Badge>
        ),
    },
    {
      key: 'actions',
      header: '',
      align: 'right',
      width: 50,
      render: (t) =>
        t.revoked_at_ms ? null : <Button size="xs" variant="ghost" icon={<TbTrash />} title="Revoke" aria-label={`Revoke ${t.name}`} onClick={() => revokeToken(t)} />,
    },
  ]

  return (
    <>
      <CollapsiblePanel
        title="Users"
        persistKey="ot.panel.settings.users"
        titleActions={<InfoTip label="Users">{ROLES_INFO}</InfoTip>}
        actions={
          <Button size="sm" icon={<TbPlus />} onClick={() => setAdding(true)}>
            Add account
          </Button>
        }
      >
        <div className="panel-body">
          {accounts === null ? (
            <span className="muted">LOADING…</span>
          ) : (
            <DataTable aria-label="Accounts" columns={columns} rows={accounts} rowKey={(a) => a.id} density="compact" empty="No accounts." defaultSort={{ key: 'email', direction: 'asc' }} />
          )}
        </div>
      </CollapsiblePanel>
      <CollapsiblePanel
        title="API tokens"
        persistKey="ot.panel.settings.tokens"
        titleActions={<InfoTip label="API tokens">For scripts and services: each acts as an account, with its role, until it expires or is revoked.</InfoTip>}
        actions={
          <Button size="sm" icon={<TbPlus />} disabled={!accounts} onClick={() => setMakingToken(true)}>
            New token
          </Button>
        }
      >
        <div className="panel-body">
          {tokens === null ? (
            <span className="muted">LOADING…</span>
          ) : (
            <DataTable aria-label="API tokens" columns={tokenColumns} rows={tokens} rowKey={(t) => t.jti} density="compact" empty="No API tokens." defaultSort={{ key: 'created', direction: 'desc' }} />
          )}
        </div>
      </CollapsiblePanel>
      {adding && (
        <AddAccount
          onClose={() => setAdding(false)}
          onAdded={() => {
            setAdding(false)
            load()
          }}
        />
      )}
      {resetting && (
        <ResetPassword
          account={resetting}
          onClose={() => setResetting(null)}
          onDone={() => {
            setResetting(null)
            load()
          }}
        />
      )}
      {makingToken && accounts && <NewToken accounts={accounts} onClose={() => setMakingToken(false)} onMade={load} />}
    </>
  )
}
