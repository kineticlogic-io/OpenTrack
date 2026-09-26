import { useEffect, useState, type ReactNode } from 'react'
import { motion } from 'framer-motion'
import { Button } from 'staresdk'
import { api, type WarningBanner } from '../api/client'
import { ackKey, useAuth } from './context'

function acked(id: string) {
  try {
    return sessionStorage.getItem(ackKey(id)) === '1'
  } catch {
    return false
  }
}

function ack(id: string) {
  try {
    sessionStorage.setItem(ackKey(id), '1')
  } catch {
    /* storage unavailable: asked again next load */
  }
}

/**
 * The warning users accept after signing in (Settings → Banners), as OpenStare's: AGREE goes on,
 * DECLINE signs out. Accepted once per browser tab session and account.
 */
export function WarnGate({ children, banner }: { children: ReactNode; banner?: ReactNode }) {
  const { user, hasRole, logout, authOn } = useAuth()
  const [warning, setWarning] = useState<WarningBanner | null | undefined>(undefined)
  const [accepted, setAccepted] = useState(() => (user ? acked(user.id) : false))
  const [leaving, setLeaving] = useState(false)

  useEffect(() => {
    if (!authOn || accepted) return
    api.warningBanner().then(setWarning, () => setWarning(null))
  }, [authOn, accepted])

  if (!authOn || !user || accepted) return <>{children}</>
  if (warning === undefined) return null
  if (!warning || !warning.enabled || !warning.text.trim()) return <>{children}</>

  const agree = () => {
    ack(user.id)
    setAccepted(true)
  }
  const decline = async () => {
    setLeaving(true)
    await logout()
  }
  const toSettings = () => {
    ack(user.id)
    window.location.hash = 'settings'
    setAccepted(true)
  }

  return (
    <>
    {banner}
    <div className="auth-screen">
      <div className="auth-column wide">
        <div className="auth-wordmark">OpenTrack</div>
        <div className="auth-subtitle">WARNING — ACKNOWLEDGMENT REQUIRED</div>
        <motion.div className="auth-card warn" initial={{ opacity: 0, y: 16 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.4, ease: 'easeOut' }}>
          <div className="warn-text">{warning.text}</div>
          <div className="warn-actions">
            <Button type="button" variant="primary" size="lg" autoFocus onClick={agree} style={{ flex: 1 }}>
              AGREE
            </Button>
            <Button type="button" variant="secondary" size="lg" onClick={decline} disabled={leaving} aria-busy={leaving} style={{ flex: 1 }}>
              {leaving ? 'SIGNING OUT…' : 'DECLINE'}
            </Button>
          </div>
          {hasRole('admin') && (
            <Button type="button" variant="ghost" size="sm" onClick={toSettings} style={{ alignSelf: 'center' }}>
              Go to Settings
            </Button>
          )}
        </motion.div>
      </div>
    </div>
    </>
  )
}
