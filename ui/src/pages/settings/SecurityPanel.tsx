import { useEffect, useState } from 'react'
import { TbFileImport, TbPlus, TbX } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, FieldSelect, Input, SaveButton, Toggle, useToast } from 'staresdk'
import { api, ROLES, type AuthSettings, type AuthSettingsResponse, type Role, type RoleMap } from '../../api/client'
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

/** Settings → Security: session length, password sign-in, SAML, OpenStare sign-in, client certificates (admins only). */
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
          <SettingsRow label="Session length (hours)" hint="How long a sign-in lasts. Empty: 24 hours. Between 0.25 and 720.">
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
              <SettingsRow
                label="Enabled"
                hint="On: the sign-in page offers single sign-on through the identity provider below. It needs the IdP entity ID, sign-in URL and signing certificate, and OT_PUBLIC_URL. Off: SAML sign-ins are refused; accounts made by SAML stay."
              >
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
              <SettingsRow label="IdP entity ID" hint="The identity provider's own name for itself (the entityID in its metadata), usually a URL. Read metadata fills it; sign-on itself works from the pasted metadata, so to change the provider, paste and read its metadata again.">
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
          <SettingsRow
            label="Enabled"
            hint="On: an OpenStare session or API token signs in here, checked with OpenStare at the API URL below. Off: only OpenTrack's own sign-ins count."
          >
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
    </>
  )
}
