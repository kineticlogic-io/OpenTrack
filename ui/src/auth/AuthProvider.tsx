import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useToast } from 'staresdk'
import { api, ApiError, type AuthPublic, type Me, type Role } from '../api/client'
import { AuthCtx, ackKey, here, roleAtLeast } from './context'

/** Calls whose 401 is an answer (wrong password, not signed in yet), not a lapsed session. */
const QUIET_401 = ['/api/v1/auth/login', '/api/v1/auth/me', '/api/v1/auth/password', '/api/v1/auth/public']

type Wrapped = typeof window.fetch & { __otAuth?: boolean }

/**
 * Who is signed in, loaded from /auth/me at start. Any other /api call answered 401 means the
 * session ended: go to the sign-in page, coming back here after. A 403 (role too low) is shown as
 * a toast with the server's reason.
 */
export function AuthProvider({ children }: { children: ReactNode }) {
  const { toast } = useToast()
  const [user, setUser] = useState<Me | null>(null)
  const [config, setConfig] = useState<AuthPublic | null>(null)
  const [loading, setLoading] = useState(true)
  const toastRef = useRef(toast)
  useEffect(() => {
    toastRef.current = toast
  }, [toast])

  useEffect(() => {
    const original = window.fetch
    if ((original as Wrapped).__otAuth) return
    const wrapped: Wrapped = async (input, init) => {
      const res = await original(input, init)
      const url = typeof input === 'string' ? input : input instanceof URL ? input.pathname : input.url
      const path = url.startsWith('http') ? new URL(url).pathname : url
      if (!path.startsWith('/api/')) return res
      if (res.status === 401 && !QUIET_401.some((p) => path.startsWith(p)) && window.location.pathname !== '/login') {
        window.location.assign('/login?from=' + encodeURIComponent(here()))
      } else if (res.status === 403 && !path.startsWith('/api/v1/auth/login')) {
        res
          .clone()
          .json()
          .then(
            (b: { error?: string }) => b.error,
            () => undefined,
          )
          .then((m) => toastRef.current({ variant: 'error', title: 'Not allowed', message: m ?? 'Your role does not allow this.', dedupeKey: `403:${m}` }))
      }
      return res
    }
    wrapped.__otAuth = true
    window.fetch = wrapped
    return () => {
      window.fetch = original
    }
  }, [])

  useEffect(() => {
    let cancelled = false
    Promise.allSettled([api.authPublic(), api.me()]).then(([pub, me]) => {
      if (cancelled) return
      if (pub.status === 'fulfilled') setConfig(pub.value)
      if (me.status === 'fulfilled') setUser(me.value)
      else if (!(me.reason instanceof ApiError && me.reason.status === 401)) console.warn('auth/me', me.reason)
      setLoading(false)
    })
    return () => {
      cancelled = true
    }
  }, [])

  const login = useCallback(async (email: string, password: string) => {
    // The caller loads the page it returns to, which reads the new session.
    await api.login(email, password)
  }, [])

  const logout = useCallback(async () => {
    const id = user?.id
    try {
      await api.logout()
    } catch {
      /* signed out here whatever the server says */
    } finally {
      if (id) {
        try {
          sessionStorage.removeItem(ackKey(id))
        } catch {
          /* storage unavailable */
        }
      }
      // A full load of the sign-in page, rather than clearing `user` here: that would race the
      // guard's own redirect to /login?from=…
      window.location.assign('/login')
    }
  }, [user?.id])

  const authOn = config ? config.auth : user?.via !== 'disabled'
  const hasRole = useCallback((min: Role) => roleAtLeast(user?.role, min), [user?.role])

  const value = useMemo(() => ({ user, config, authOn, loading, hasRole, login, logout, setUser }), [user, config, authOn, loading, hasRole, login, logout])
  return <AuthCtx.Provider value={value}>{children}</AuthCtx.Provider>
}
