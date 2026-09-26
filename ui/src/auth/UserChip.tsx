import { useRef, useState } from 'react'
import { TbKey, TbLogout, TbUserCircle } from 'react-icons/tb'
import { Badge, Button, ContextMenu, ContextMenuItem, Input, Label, Modal, useToast } from 'staresdk'
import { api } from '../api/client'
import { errorMessage } from '../lib/format'
import { ROLE_LABEL, useAuth } from './context'

const VIA: Record<string, string> = {
  session: 'Password or single sign-on',
  api_token: 'API token',
  client_cert: 'Client certificate',
  openstare: 'OpenStare',
  disabled: 'Sign-in is off',
}

function ChangePassword({ onClose }: { onClose: () => void }) {
  const { setUser } = useAuth()
  const { toast } = useToast()
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [again, setAgain] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (next !== again) return setError('The new passwords differ.')
    setBusy(true)
    setError(null)
    try {
      setUser(await api.changePassword(current, next))
      toast({ variant: 'success', title: 'Password changed', message: 'Your other sessions were signed out.' })
      onClose()
    } catch (err) {
      setError(errorMessage(err))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title="Change password" onClose={onClose} width={420} resizable={false}>
      <form className="panel-body stack" onSubmit={submit}>
        <div className="auth-field">
          <Label size="sm" htmlFor="pw-current">Current password</Label>
          <Input id="pw-current" type="password" autoComplete="current-password" autoFocus required value={current} onChange={(e) => setCurrent(e.target.value)} />
        </div>
        <div className="auth-field">
          <Label size="sm" htmlFor="pw-new">New password</Label>
          <Input id="pw-new" type="password" autoComplete="new-password" required value={next} onChange={(e) => setNext(e.target.value)} />
        </div>
        <div className="auth-field">
          <Label size="sm" htmlFor="pw-again">New password again</Label>
          <Input id="pw-again" type="password" autoComplete="new-password" required value={again} onChange={(e) => setAgain(e.target.value)} />
        </div>
        {error && <div className="auth-error">{error}</div>}
        <div className="num-row" style={{ justifyContent: 'flex-end' }}>
          <Button size="sm" variant="ghost" type="button" onClick={onClose}>
            Cancel
          </Button>
          <Button size="sm" variant="primary" type="submit" disabled={busy || !current || !next}>
            Change password
          </Button>
        </div>
      </form>
    </Modal>
  )
}

/** The signed-in account in the header: who, role, change password, sign out. */
export function UserChip() {
  const { user, authOn, logout } = useAuth()
  const ref = useRef<HTMLDivElement>(null)
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null)
  const [changing, setChanging] = useState(false)
  if (!user || !authOn) return null
  const open = () => {
    if (menu) return setMenu(null)
    const r = ref.current?.getBoundingClientRect()
    if (r) setMenu({ x: r.right - 260, y: r.bottom + 4 })
  }
  const signOut = async () => {
    setMenu(null)
    await logout()
  }
  return (
    <div ref={ref} onMouseDown={(e) => e.stopPropagation()}>
      <Button size="sm" variant="ghost" icon={<TbUserCircle />} onClick={open} aria-haspopup="menu" aria-expanded={menu != null} title={user.email}>
        {user.name || user.email}
      </Button>
      {menu && (
        <ContextMenu x={menu.x} y={menu.y} width={260} estimatedHeight={160} onClose={() => setMenu(null)} ariaLabel="Account" header={VIA[user.via] ?? user.via}>
          <div className="user-chip-head">
            {user.name && <div className="user-chip-name">{user.name}</div>}
            <div className="muted">{user.email}</div>
            <Badge size="sm" color={user.role === 'admin' ? 'brand' : user.role === 'track_manager' ? 'blue' : 'grey'}>
              {ROLE_LABEL[user.role]}
            </Badge>
          </div>
          {user.can_change_password && (
            <ContextMenuItem
              onClick={() => {
                setMenu(null)
                setChanging(true)
              }}
            >
              <span className="num-row">
                <TbKey /> Change password
              </span>
            </ContextMenuItem>
          )}
          {user.via === 'session' ? (
            <ContextMenuItem danger onClick={signOut}>
              <span className="num-row">
                <TbLogout /> Sign out
              </span>
            </ContextMenuItem>
          ) : null}
        </ContextMenu>
      )}
      {changing && <ChangePassword onClose={() => setChanging(false)} />}
    </div>
  )
}
