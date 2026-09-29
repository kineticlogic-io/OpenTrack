# Deployment hardening

This is the checklist for deploying OpenTrack 0.4.1 to an accredited environment. The controls
themselves are described in the administrator guide, under
[Security hardening](../guides/admin.md#security-hardening). How they map to NIST 800-53 is in
[stig-mapping.md](stig-mapping.md).

## Before first start

1. **Use a signed image.** Verify a release image before running it:
   `cosign verify --key cosign.pub --insecure-ignore-tlog=true ghcr.io/phornstein/opentrack@<digest>`
   ([supply-chain.md](supply-chain.md)).
2. **Use the image.** Only the image has OpenSSL's FIPS provider, so only it has SAML under FIPS.
   Its base images are pinned by digest. Scan the built image with your scanner (for example
   `grype` or `trivy`) and keep the report ([supply-chain.md](supply-chain.md)).
3. **Run it hardened.** `docker-compose.yml` already sets:
   - `read_only: true`, with `tmpfs` for `/tmp` and `/home/opentrack`;
   - `cap_drop: [ALL]`;
   - `security_opt: no-new-privileges`.

   Keep them. The only writable volume is `/data`.
4. **Set the first admin** with `OT_ADMIN_EMAIL` and a policy-compliant `OT_ADMIN_PASSWORD`, or use
   the temporary password in `initial-admin.txt`. The first sign-in forces a change either way.
   Then delete `initial-admin.txt`.
5. **Set `OT_SESSION_SECRET`** (32 characters or more) from your secret store, or protect
   `session.key` in the data directory. Anyone with it can mint sessions.
6. **Never set `OT_AUTH=off`** outside a laboratory. It makes every caller an admin; the server
   warns every minute and the UI shows a red banner.

## Encrypt every link

| Link | Settings |
|---|---|
| Browsers and API | `OT_TLS_CERT` and `OT_TLS_KEY` (optionally `OT_TLS_CLIENT_CA` for CAC/PKI client certificates, with `OT_TLS_CLIENT_CRL` pointing at a directory of your CAs' CRLs, refreshed daily), **or** a TLS proxy with `OT_PUBLIC_TLS=1` / an `https://` `OT_PUBLIC_URL`, so cookies are `Secure` and HSTS is sent |
| NATS | a `tls://` URL, `OT_NATS_CA`, and `OT_NATS_CERT` / `OT_NATS_KEY` for mutual TLS; prefer `.creds` or mTLS over a shared token |
| Redis | a `rediss://` URL, `OT_REDIS_CA`, `OT_REDIS_CERT` / `OT_REDIS_KEY` (Redis `tls-auth-clients yes`) |
| Feeds | each source's transport TLS settings; don't use `insecure_skip_verify` |
| TAK output | Settings → TAK output: a TAK Server over TLS (8089) with a client certificate; a listening output with TLS and a **client CA** (and revocation lists) so only enrolled TAK clients connect |

**TAK outputs in plaintext expose the picture.** A plain TCP output (to a TAK Server's 8087 or a
listening output without TLS) sends every published track in the clear, and a plain listening
output lets anyone who can reach its port take the whole picture. **Multicast is always
plaintext**: anyone on the network segment (or as far as its TTL reaches) can read it. Use them
only on a network that is itself protected and accredited for the picture's classification, and
document the risk acceptance; keep multicast TTL at 1. The TAK output panel marks each output TLS or
plaintext, and the `cot` role logs a warning when a plaintext output starts. OpenTrack sends
events as the tracks are, without adding classification markings to them.

TLS runs only FIPS-approved suites (docs/security/fips.md).

## Sign-in

- **SAML:** the identity provider signs with RSA-SHA256 or stronger. Keep **Allow admin** off
  unless the identity provider should make admins.
- **Local accounts:** SAML never signs in to a local account (a matching email is refused), so
  keep local accounts to break-glass and service use.
- **Password sign-in:** with single sign-on in place, consider turning **Password sign-in** off
  (Settings → Security → Single sign-on). Keep one break-glass local admin, with **Never turn
  off** on its row in Settings → Users so inactivity doesn't turn it off.
- **Notice and consent:** turn on the DoD notice and consent banner (Settings → Banners) with
  your standard text, and the classification banner.

## Fixed values

The account policy is fixed at the STIG values; there is nothing to set:
- **Passwords:** 15 characters or more, all four character classes, 8 characters changed, 5
  remembered, a minimum age of 24 h and a maximum of 60 days.
- **Lockout:** 3 failures in 15 minutes lock the account for 15 minutes.
- **Sessions:** idle timeout 15 minutes (admins 10), 3 per account, 24 hours at most.
- **Inactivity:** accounts are turned off after 35 days, except those marked **Never turn off**.
- **Audit record:** kept forever.

## Operate

- **Review the audit record** at the interval your plan sets, in your SIEM: ship the JSON logs
  (`OT_LOG_FORMAT=json`). Every audit row is logged as an `audit record` event (target `audit`),
  and the hourly chain head is logged too. The API gives the record itself to admins
  (`GET /api/v1/audit`, CSV with `format=csv`); check the chain with `GET /api/v1/audit/verify`.
- **Keep logs off the host.** A copy of the chain head outside the database is what shows that
  the audit tail was cut.
- **Back up** SQLite with `sqlite3 .backup` (online), and Redis. Protect backups like the data
  directory: it holds password hashes and the session key. See the administrator guide.
- **Least privilege:** viewers for monitoring and read-only services; few admins; one API token
  per service, with an expiry.
- **Keep secrets out of source specs** (`${env:NAME}`). Only admins see secret fields, but inline
  secrets still sit in the database and its backups.
- **Patch:** CI runs `cargo deny` (RustSec) and `npm audit`. Rebuild the image when either reports
  something, and when Debian bookworm ships security updates.
