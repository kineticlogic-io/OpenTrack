# Deployment hardening

This is the checklist for deploying OpenTrack 0.4.0 to an accredited environment. The controls
themselves are described in the administrator guide, under
[Security hardening](../guides/admin.md#security-hardening). How they map to NIST 800-53 is in
[stig-mapping.md](stig-mapping.md).

## Before first start

1. **Use the image.** Only the image has OpenSSL's FIPS provider, so only it has SAML under FIPS.
   Its base images are pinned by digest. Scan the built image with your scanner (for example
   `grype` or `trivy`) and keep the report ([supply-chain.md](supply-chain.md)).
2. **Run it hardened.** `docker-compose.yml` already sets:
   - `read_only: true`, with `tmpfs` for `/tmp` and `/home/opentrack`;
   - `cap_drop: [ALL]`;
   - `security_opt: no-new-privileges`.

   Keep them. The only writable volume is `/data`.
3. **Set the first admin** with `OT_ADMIN_EMAIL` and a policy-compliant `OT_ADMIN_PASSWORD`, or use
   the temporary password in `initial-admin.txt`. The first sign-in forces a change either way.
   Then delete `initial-admin.txt`.
4. **Set `OT_SESSION_SECRET`** (32 characters or more) from your secret store, or protect
   `session.key` in the data directory. Anyone with it can mint sessions.
5. **Never set `OT_AUTH=off`** outside a laboratory. It makes every caller an admin; the server
   warns every minute and the UI shows a red banner.

## Encrypt every link

| Link | Settings |
|---|---|
| Browsers and API | `OT_TLS_CERT` and `OT_TLS_KEY` (optionally `OT_TLS_CLIENT_CA` for CAC/PKI client certificates), **or** a TLS proxy with `OT_PUBLIC_TLS=1` / an `https://` `OT_PUBLIC_URL`, so cookies are `Secure` and HSTS is sent |
| NATS | a `tls://` URL, `OT_NATS_CA`, and `OT_NATS_CERT` / `OT_NATS_KEY` for mutual TLS; prefer `.creds` or mTLS over a shared token |
| Redis | a `rediss://` URL, `OT_REDIS_CA`, `OT_REDIS_CERT` / `OT_REDIS_KEY` (Redis `tls-auth-clients yes`) |
| Feeds | each source's transport TLS settings; don't use `insecure_skip_verify` |

TLS runs only FIPS-approved suites (docs/security/fips.md).

## Sign-in

- **SAML:** the identity provider signs with RSA-SHA256 or stronger. Keep **Allow admin** off
  unless the identity provider should make admins.
- **Local accounts:** don't create a local account for anyone who should come through SAML. A
  SAML sign-in whose email matches a local account signs in *as* that account, with its role
  (open finding F-1 in [stig-mapping.md](stig-mapping.md)).
- **Password sign-in:** with single sign-on in place, consider **Disable password sign-in**. Keep
  one break-glass local admin, exempt from inactivity (**Never turn off**).
- **Notice and consent:** turn on the DoD notice and consent banner (Settings → Banners) with
  your standard text, and the classification banner.

## Keep the defaults

The STIG values are the defaults in **Settings → Security**:
- **Passwords:** 15 characters or more, all four character classes, 8 characters changed, 5
  remembered, a minimum age of 24 h and a maximum of 60 days.
- **Lockout:** 3 failures in 15 minutes lock the account for 15 minutes.
- **Sessions:** idle timeout 15 minutes (admins 10), 3 per account.
- **Inactivity:** accounts are turned off after 35 days.

Loosening any of them needs a documented risk acceptance.

## Operate

- **Review the audit record** (Settings → Audit) at the interval your plan sets. Run **Verify
  chain**. Export CSV for your SIEM, or ship the JSON logs (`OT_LOG_FORMAT=json`), which also carry
  the hourly chain head.
- **Keep logs off the host.** A copy of the chain head outside the database is what shows that
  the audit tail was cut.
- **Back up** SQLite with `sqlite3 .backup` (online), and Redis. Protect backups like the data
  directory: it holds password hashes and the session key. See the administrator guide.
- **Least privilege:** viewers for monitoring and read-only services; few admins; one API token
  per service, with an expiry.
- **Keep secrets out of source specs** (`${env:NAME}`). Every signed-in user can read source specs
  (open finding F-2).
- **Patch:** CI runs `cargo deny` (RustSec) and `npm audit`. Rebuild the image when either reports
  something, and when Debian bookworm ships security updates.
