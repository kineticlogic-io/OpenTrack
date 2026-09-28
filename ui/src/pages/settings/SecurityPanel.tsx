import { useEffect, useState } from 'react'
import { TbFileImport, TbPlus, TbX } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, FieldSelect, Input, SaveButton, Toggle, useToast } from 'staresdk'
import { api, ROLES, type AuthSettings, type AuthSettingsResponse, type CorrelationSettings, type Role, type RoleMap } from '../../api/client'
import { InfoTip } from '../../components/InfoTip'
import { errorMessage } from '../../lib/format'
import { INPUT } from '../../lib/valueSpec'
import { SettingsRow } from './SettingsRow'

const ROLE_FIELDS = ROLES.map((name) => ({ name }))
const asRole = (v: string | null): Role => (ROLES as readonly string[]).includes(v ?? '') ? (v as Role) : 'viewer'

/** Rows of value → role (the first match wins). */
function RoleMapping({ rows, onChange, placeholder, label }: { rows: RoleMap[]; onChange: (rows: RoleMap[]) => void; placeholder: string; label: string }) {
  const set = (i: number, patch: Partial<RoleMap>) => onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)))
  return (
    <div className="map-rows">
      {rows.map((r, i) => (
        <div key={i} className="num-row">
          <Input style={{ ...INPUT, width: 200 }} aria-label={`${label} value ${i + 1}`} placeholder={placeholder} value={r.value} onChange={(e) => set(i, { value: e.target.value })} />
          <span className="muted">→</span>
          <FieldSelect ariaLabel={`${label} role ${i + 1}`} fields={ROLE_FIELDS} value={r.role} onChange={(v) => set(i, { role: asRole(v) })} style={{ width: 140 }} />
          <Button size="xs" variant="ghost" icon={<TbX />} aria-label={`Remove ${label} row ${i + 1}`} title="Remove" onClick={() => onChange(rows.filter((_, j) => j !== i))} />
        </div>
      ))}
      <div>
        <Button size="sm" variant="ghost" icon={<TbPlus />} onClick={() => onChange([...rows, { value: '', role: 'viewer' }])}>
          Add row
        </Button>
      </div>
    </div>
  )
}

/** A number field; empty gives `empty` (when the setting has an empty meaning). */
function Num({ label, value, onChange, min, max, step = 1, width = 90, placeholder }: {
  label: string
  value: number | null | undefined
  onChange: (v: number | null) => void
  min?: number
  max?: number
  step?: number | 'any'
  width?: number
  placeholder?: string
}) {
  return (
    <Input
      style={{ ...INPUT, width }}
      type="number"
      min={min}
      max={max}
      step={step}
      aria-label={label}
      placeholder={placeholder}
      value={value ?? ''}
      onChange={(e) => onChange(e.target.value === '' ? null : Number(e.target.value))}
    />
  )
}

/** Emails as comma-separated text (kept as typed; the list is what it parses to). */
function EmailList({ value, onChange }: { value: string[]; onChange: (v: string[]) => void }) {
  const [text, setText] = useState(value.join(', '))
  return (
    <Input
      style={{ ...INPUT, width: 420 }}
      aria-label="Accounts exempt from inactivity"
      placeholder="breakglass@example.org"
      value={text}
      spellCheck={false}
      onChange={(e) => {
        setText(e.target.value)
        onChange(
          e.target.value
            .split(',')
            .map((x) => x.trim())
            .filter(Boolean),
        )
      }}
    />
  )
}

/**
 * Settings → Security → Security labels: the classification order a fused track's label is
 * chosen by. It is a correlation setting (the engine applies it), saved on its own.
 */
function LabelOrder() {
  const { toast } = useToast()
  const [settings, setSettings] = useState<CorrelationSettings | null>(null)
  const [text, setText] = useState('')
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)
  const order = (s: CorrelationSettings | null) => s?.labels?.classification_order ?? ['UNCLASSIFIED', 'CUI', 'CONFIDENTIAL', 'SECRET', 'TOP SECRET']
  useEffect(() => {
    api.correlationSettings().then(
      (r) => {
        setSettings(r.settings)
        setText(order(r.settings).join('\n'))
      },
      (e) => toast({ variant: 'error', title: 'Security labels', message: errorMessage(e) }),
    )
  }, [toast])
  const lines = text
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean)
  const dirty = settings != null && JSON.stringify(lines) !== JSON.stringify(order(settings))
  const save = async () => {
    if (!settings) return
    setSaving(true)
    setSaved(false)
    try {
      // Read again just before: the rest of the correlation settings stay as they are now.
      const now = await api.correlationSettings()
      const r = await api.saveCorrelationSettings({ ...now.settings, labels: { classification_order: lines } })
      setSettings(r.settings)
      setText(order(r.settings).join('\n'))
      setSaved(true)
    } catch (e) {
      toast({ variant: 'error', title: 'Not saved', message: errorMessage(e) })
    } finally {
      setSaving(false)
    }
  }
  return (
    <CollapsiblePanel
      title="Security labels"
      persistKey="ot.panel.settings.labels"
      titleActions={
        <InfoTip label="Security labels">
          A track fused from several sources is marked with the highest classification of theirs in this order, every restriction any of them has, and
          only the releasability they all share (the intersection of their comma-separated lists; NONE when they share none). A classification not in
          the list ranks above all of them, so a track is never marked too low.
        </InfoTip>
      }
      actions={<SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />}
    >
      <div className="panel-body stack">
        <SettingsRow label="Classification order" hint="One per line, lowest first. Case does not matter; U, C, S and TS stand for their names, and caveats after // are ignored when ranking.">
          <textarea className="plain-textarea" style={{ maxWidth: 320 }} aria-label="Classification order" rows={6} spellCheck={false} value={text} onChange={(e) => setText(e.target.value)} />
        </SettingsRow>
      </div>
    </CollapsiblePanel>
  )
}

/** Settings → Security: sessions, password sign-in and its policy, lockout, inactivity, audit retention, SAML, OpenStare sign-in, client certificates, security labels (admins only). */
export function SecurityPanel() {
  const { toast } = useToast()
  const [loaded, setLoaded] = useState<AuthSettingsResponse | null>(null)
  const [draft, setDraft] = useState<AuthSettings | null>(null)
  const [metadata, setMetadata] = useState('')
  const [parsing, setParsing] = useState(false)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)

  const take = (r: AuthSettingsResponse) => {
    setLoaded(r)
    const { build: _build, ...settings } = r
    setDraft(settings)
    setMetadata(settings.saml.idp_metadata_xml)
  }
  useEffect(() => {
    api.authSettings().then(take, (e) => toast({ variant: 'error', title: 'Security', message: errorMessage(e) }))
  }, [toast])

  if (!loaded || !draft) return <span className="muted">LOADING…</span>
  const { build } = loaded
  const { build: _b, ...original } = loaded
  const dirty = JSON.stringify(draft) !== JSON.stringify(original)
  const saml = draft.saml
  const os = draft.openstare
  const pw = draft.password
  const setPw = (patch: Partial<AuthSettings['password']>) => setDraft((d) => d && { ...d, password: { ...d.password, ...patch } })
  const lock = draft.lockout
  const setLock = (patch: Partial<AuthSettings['lockout']>) => setDraft((d) => d && { ...d, lockout: { ...d.lockout, ...patch } })
  const sess = draft.sessions
  const setSess = (patch: Partial<AuthSettings['sessions']>) => setDraft((d) => d && { ...d, sessions: { ...d.sessions, ...patch } })
  const inact = draft.inactivity
  const setInact = (patch: Partial<AuthSettings['inactivity']>) => setDraft((d) => d && { ...d, inactivity: { ...d.inactivity, ...patch } })
  // Functional updates: the metadata reply lands after an await, when `draft` may be stale.
  const setSaml = (patch: Partial<AuthSettings['saml']>) => setDraft((d) => d && { ...d, saml: { ...d.saml, ...patch } })
  const setOs = (patch: Partial<AuthSettings['openstare']>) => setDraft((d) => d && { ...d, openstare: { ...d.openstare, ...patch } })

  const save = async () => {
    setSaving(true)
    setSaved(false)
    try {
      take(await api.saveAuthSettings(draft))
      setSaved(true)
    } catch (e) {
      toast({ variant: 'error', title: 'Not saved', message: errorMessage(e) })
    } finally {
      setSaving(false)
    }
  }
  const parse = async () => {
    setParsing(true)
    try {
      const m = await api.parseSamlMetadata(metadata)
      setSaml({ idp_metadata_xml: m.idp_metadata_xml, idp_entity_id: m.idp_entity_id, sso_url: m.sso_url, signing_cert: m.signing_cert })
      toast({ variant: 'success', title: 'Metadata read', message: m.idp_entity_id })
    } catch (e) {
      toast({ variant: 'error', title: 'Metadata not read', message: errorMessage(e) })
    } finally {
      setParsing(false)
    }
  }
  const saveButton = <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />
  const base = build.public_url?.replace(/\/+$/, '')

  return (
    <>
      <CollapsiblePanel title="Sign-in" persistKey="ot.panel.settings.signin" actions={saveButton}>
        <div className="panel-body stack">
          <SettingsRow label="Session length (hours)" hint="How long a sign-in lasts at most, used or not. Empty: 24 hours. Between 0.25 and 720.">
            <Input
              style={{ ...INPUT, width: 100 }}
              type="number"
              min={0.25}
              max={720}
              step="any"
              aria-label="Session length in hours"
              placeholder="24"
              value={draft.session_hours ?? ''}
              onChange={(e) => {
                const { session_hours: _h, ...rest } = draft
                setDraft(e.target.value === '' ? rest : { ...draft, session_hours: Number(e.target.value) })
              }}
            />
          </SettingsRow>
          <SettingsRow
            label="Password sign-in"
            hint="Off: accounts sign in only with SAML or OpenStare. To turn it off, sign in with single sign-on first, so a working way back is proven."
          >
            <Toggle size="sm" aria-label="Password sign-in" value={!draft.disable_password_login} onChange={(on) => setDraft({ ...draft, disable_password_login: !on })} />
          </SettingsRow>
          <SettingsRow label="Idle timeout (minutes)" hint="A browser session not used for this long ends; the page's own refreshes do not count as use. Between 1 and 1440. API tokens have no idle timeout.">
            <Num label="Idle timeout in minutes" value={sess.idle_minutes} min={1} max={1440} step="any" onChange={(v) => setSess({ idle_minutes: v ?? 15 })} />
          </SettingsRow>
          <SettingsRow label="Admin idle timeout (minutes)" hint="The same for admins, usually shorter. Empty: as everyone's.">
            <Num label="Admin idle timeout in minutes" value={sess.admin_idle_minutes} min={1} max={1440} step="any" placeholder="same" onChange={(v) => setSess({ admin_idle_minutes: v })} />
          </SettingsRow>
          <SettingsRow label="Sessions per account" hint="How many browsers an account may be signed in from at once; a new sign-in past it ends the oldest. 0: no limit.">
            <Num label="Sessions per account" value={sess.max_per_account} min={0} max={100} onChange={(v) => setSess({ max_per_account: v ?? 0 })} />
          </SettingsRow>
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel
        title="Password policy"
        persistKey="ot.panel.settings.password-policy"
        titleActions={
          <InfoTip label="Password policy">
            For accounts with a password (not single sign-on). The rules apply when a password is set: existing passwords keep working until they change or
            expire. The defaults follow the DoD application security STIG.
          </InfoTip>
        }
        actions={saveButton}
      >
        <div className="panel-body stack">
          <SettingsRow label="Minimum length" hint="Characters, at least 8.">
            <Num label="Minimum password length" value={pw.min_length} min={8} max={128} onChange={(v) => setPw({ min_length: v ?? 15 })} />
          </SettingsRow>
          <SettingsRow label="Must contain" hint="A special character is anything that is not a letter or a digit.">
            <div className="num-row">
              {(
                [
                  ['require_upper', 'Upper case'],
                  ['require_lower', 'Lower case'],
                  ['require_digit', 'Digit'],
                  ['require_special', 'Special'],
                ] as const
              ).map(([k, label]) => (
                <span key={k} className="num-row">
                  <Toggle size="sm" aria-label={`Require ${label.toLowerCase()}`} value={pw[k]} onChange={(on) => setPw({ [k]: on })} />
                  <span className="muted">{label}</span>
                </span>
              ))}
            </div>
          </SettingsRow>
          <SettingsRow label="Remembered passwords" hint="A new password may not be any of this many last ones. 0: any. At most 24.">
            <Num label="Password history" value={pw.history} min={0} max={24} onChange={(v) => setPw({ history: v ?? 0 })} />
          </SettingsRow>
          <SettingsRow label="Characters to change" hint="When you change your own password, at least this many characters must differ from the current one. 0: any change.">
            <Num label="Characters to change" value={pw.min_changed_chars} min={0} max={128} onChange={(v) => setPw({ min_changed_chars: v ?? 0 })} />
          </SettingsRow>
          <SettingsRow label="Minimum age (hours)" hint="How soon you may change your own password again. An admin's reset and a required change are exempt. 0: any time.">
            <Num label="Minimum password age in hours" value={pw.min_age_hours} min={0} max={720} step="any" onChange={(v) => setPw({ min_age_hours: v ?? 0 })} />
          </SettingsRow>
          <SettingsRow label="Maximum age (days)" hint="After this, the password must change at the next sign-in. 0: never expires.">
            <Num label="Maximum password age in days" value={pw.max_age_days} min={0} max={3650} step="any" onChange={(v) => setPw({ max_age_days: v ?? 0 })} />
          </SettingsRow>
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel
        title="Lockout and inactivity"
        persistKey="ot.panel.settings.lockout"
        titleActions={
          <InfoTip label="Lockout and inactivity">
            Failed password sign-ins lock an account for a while; accounts nobody signs in to are turned off. A locked or turned-off account gets the same
            answer as a wrong password. Users → Unlock, or Active, lets it in again.
          </InfoTip>
        }
        actions={saveButton}
      >
        <div className="panel-body stack">
          <SettingsRow label="Failed sign-ins to lock" hint="Consecutive failures within the window that lock the account. 0: never lock.">
            <Num label="Failed sign-ins to lock" value={lock.max_failures} min={0} max={100} onChange={(v) => setLock({ max_failures: v ?? 0 })} />
          </SettingsRow>
          <SettingsRow label="Within (minutes)" hint="Failures further apart than this start the count again.">
            <Num label="Lockout window in minutes" value={lock.window_minutes} min={0} max={1440} step="any" onChange={(v) => setLock({ window_minutes: v ?? 15 })} />
          </SettingsRow>
          <SettingsRow label="Locked for (minutes)" hint="0: until an admin unlocks it (Users, or opentrack user unlock).">
            <Num label="Lock minutes" value={lock.lock_minutes} min={0} max={10080} step="any" onChange={(v) => setLock({ lock_minutes: v ?? 0 })} />
          </SettingsRow>
          <SettingsRow label="Turn off after (days)" hint="Accounts not signed in for this long are turned off, checked at sign-in and every 10 minutes; an admin turns them on again. 0: never.">
            <Num label="Inactivity days" value={inact.disable_after_days} min={0} max={3650} step="any" onChange={(v) => setInact({ disable_after_days: v ?? 0 })} />
          </SettingsRow>
          <SettingsRow label="Never turn off" hint="Break-glass accounts (emails, comma-separated) that inactivity never turns off. Keep them few and their passwords sealed.">
            <EmailList key={JSON.stringify(loaded.inactivity.exempt)} value={inact.exempt} onChange={(exempt) => setInact({ exempt })} />
          </SettingsRow>
          <SettingsRow label="Keep the audit record (days)" hint="Rows older than this are deleted (hourly); the deletion is itself recorded and the hash chain stays verifiable. 0: keep forever. Otherwise at least 7.">
            <Num label="Audit retention days" value={draft.audit.retention_days} min={0} max={36500} step="any" onChange={(v) => setDraft({ ...draft, audit: { retention_days: v ?? 0 } })} />
          </SettingsRow>
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel
        title="SAML single sign-on"
        persistKey="ot.panel.settings.saml"
        titleActions={<InfoTip label="SAML">OpenTrack is the service provider; your identity provider (Keycloak, Entra ID…) signs users in. An account is made at a user's first sign-on.</InfoTip>}
        actions={saveButton}
      >
        <div className="panel-body stack">
          {!build.saml ? (
            <SettingsRow label="SAML" hint="This build of OpenTrack was made without SAML support (the saml feature).">
              <Badge size="sm" color="grey">
                Not in this build
              </Badge>
            </SettingsRow>
          ) : (
            <>
              <SettingsRow label="Enabled">
                <Toggle size="sm" aria-label="SAML enabled" value={saml.enabled} onChange={(enabled) => setSaml({ enabled })} />
              </SettingsRow>
              <SettingsRow label="Service provider" hint="Give these to the identity provider. They come from OT_PUBLIC_URL, the address browsers use for OpenTrack.">
                {base ? (
                  <div className="stack" style={{ gap: 2 }}>
                    <span className="mono small">
                      <span className="muted">Metadata </span>
                      {base}/api/v1/auth/saml/metadata
                    </span>
                    <span className="mono small">
                      <span className="muted">ACS </span>
                      {base}/api/v1/auth/saml/acs
                    </span>
                  </div>
                ) : (
                  <span className="num-row">
                    <Badge size="sm" color="warning">
                      OT_PUBLIC_URL not set
                    </Badge>
                    <InfoTip label="OT_PUBLIC_URL">Set OT_PUBLIC_URL (e.g. https://opentrack.example.org) and restart: SAML needs it for its metadata and assertion URLs.</InfoTip>
                  </span>
                )}
              </SettingsRow>
              <SettingsRow label="IdP metadata" hint="Paste the identity provider's metadata XML and read it: it fills the entity ID, sign-in URL and certificate.">
                <div className="stack" style={{ gap: 4, width: '100%' }}>
                  <textarea className="plain-textarea" style={{ maxWidth: 560 }} aria-label="IdP metadata XML" rows={4} spellCheck={false} placeholder="<EntityDescriptor …>" value={metadata} onChange={(e) => setMetadata(e.target.value)} />
                  <div>
                    <Button size="sm" variant="ghost" icon={<TbFileImport />} disabled={parsing || !metadata.trim()} onClick={parse}>
                      Read metadata
                    </Button>
                  </div>
                </div>
              </SettingsRow>
              <SettingsRow label="IdP entity ID">
                <Input style={{ ...INPUT, width: 420 }} aria-label="IdP entity ID" value={saml.idp_entity_id} onChange={(e) => setSaml({ idp_entity_id: e.target.value })} spellCheck={false} />
              </SettingsRow>
              <SettingsRow label="IdP sign-in URL" hint="Where users are sent to sign in (HTTP-Redirect binding).">
                <Input style={{ ...INPUT, width: 420 }} aria-label="IdP sign-in URL" value={saml.sso_url} onChange={(e) => setSaml({ sso_url: e.target.value })} spellCheck={false} />
              </SettingsRow>
              <SettingsRow label="IdP signing certificate" hint="PEM or base64 DER.">
                <textarea className="plain-textarea" style={{ maxWidth: 560 }} aria-label="IdP signing certificate" rows={3} spellCheck={false} value={saml.signing_cert} onChange={(e) => setSaml({ signing_cert: e.target.value })} />
              </SettingsRow>
              <SettingsRow label="Role attribute" hint="The assertion attribute that carries the user's role.">
                <Input style={{ ...INPUT, width: 200 }} aria-label="Role attribute" value={saml.role_attribute} onChange={(e) => setSaml({ role_attribute: e.target.value })} spellCheck={false} />
              </SettingsRow>
              <SettingsRow label="Role mapping" hint="Attribute values and the OpenTrack role each gives; the first match wins. Case does not matter.">
                <RoleMapping label="SAML mapping" placeholder="attribute value" rows={saml.role_mapping} onChange={(role_mapping) => setSaml({ role_mapping })} />
              </SettingsRow>
              <SettingsRow label="Default role" hint="The role of a user whose attribute matches no row. None: they are refused.">
                <FieldSelect ariaLabel="Default role" allowNone fields={ROLE_FIELDS} value={saml.default_role} onChange={(v) => setSaml({ default_role: v == null ? null : asRole(v) })} style={{ width: 160 }} />
              </SettingsRow>
              <SettingsRow label="Allow admin" hint="Off: single sign-on gives at most track manager, whatever the mapping says.">
                <Toggle size="sm" aria-label="Allow admin by SAML" value={saml.allow_admin} onChange={(allow_admin) => setSaml({ allow_admin })} />
              </SettingsRow>
              <SettingsRow label="Button label" hint="The sign-in page's single sign-on button.">
                <Input style={{ ...INPUT, width: 240 }} aria-label="Button label" value={saml.button_label} onChange={(e) => setSaml({ button_label: e.target.value })} />
              </SettingsRow>
            </>
          )}
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel
        title="OpenStare sign-in"
        persistKey="ot.panel.settings.openstare-signin"
        titleActions={
          <InfoTip label="OpenStare sign-in">
            Let in a browser signed in to OpenStare on this host, or an OpenStare API token, with a role mapped from its OpenStare role. OpenStare checks each, so
            its sign-outs and revocations hold here too.
          </InfoTip>
        }
        actions={saveButton}
      >
        <div className="panel-body stack">
          <SettingsRow label="Enabled">
            <Toggle size="sm" aria-label="OpenStare sign-in enabled" value={os.enabled} onChange={(enabled) => setOs({ enabled })} />
          </SettingsRow>
          <SettingsRow label="API URL" hint="OpenStare's API as this server reaches it.">
            <Input style={{ ...INPUT, width: 320 }} aria-label="OpenStare API URL" value={os.api_url} onChange={(e) => setOs({ api_url: e.target.value })} spellCheck={false} />
          </SettingsRow>
          <SettingsRow label="Sign-in page URL" hint="OpenStare's sign-in page as browsers reach it, for the button on OpenTrack's sign-in page. Empty: no button.">
            <Input style={{ ...INPUT, width: 320 }} aria-label="OpenStare sign-in URL" value={os.login_url} onChange={(e) => setOs({ login_url: e.target.value })} spellCheck={false} />
          </SettingsRow>
          <SettingsRow label="Role mapping" hint="OpenStare roles (admin, operator, analyst, guest) and the OpenTrack role each gives. A role not listed is refused.">
            <RoleMapping label="OpenStare mapping" placeholder="OpenStare role" rows={os.role_mapping} onChange={(role_mapping) => setOs({ role_mapping })} />
          </SettingsRow>
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel
        title="Client certificates"
        persistKey="ot.panel.settings.certs"
        titleActions={<InfoTip label="Client certificates">With mutual TLS on the control plane, a client certificate signs in as the account listed for its subject common name (CN).</InfoTip>}
        actions={saveButton}
      >
        <div className="panel-body map-rows">
          {draft.client_certs.map((c, i) => {
            const set = (patch: Partial<typeof c>) => setDraft({ ...draft, client_certs: draft.client_certs.map((x, j) => (j === i ? { ...x, ...patch } : x)) })
            return (
              <div key={i} className="num-row">
                <Input style={{ ...INPUT, width: 240 }} aria-label={`Common name ${i + 1}`} placeholder="common name (CN)" value={c.common_name} onChange={(e) => set({ common_name: e.target.value })} spellCheck={false} />
                <span className="muted">→</span>
                <Input style={{ ...INPUT, width: 240 }} aria-label={`Account ${i + 1}`} placeholder="account email" value={c.user} onChange={(e) => set({ user: e.target.value })} spellCheck={false} />
                <Button
                  size="xs"
                  variant="ghost"
                  icon={<TbX />}
                  title="Remove"
                  aria-label={`Remove certificate ${i + 1}`}
                  onClick={() => setDraft({ ...draft, client_certs: draft.client_certs.filter((_, j) => j !== i) })}
                />
              </div>
            )
          })}
          <div>
            <Button size="sm" variant="ghost" icon={<TbPlus />} onClick={() => setDraft({ ...draft, client_certs: [...draft.client_certs, { common_name: '', user: '' }] })}>
              Add certificate
            </Button>
          </div>
        </div>
      </CollapsiblePanel>

      <LabelOrder />
    </>
  )
}
