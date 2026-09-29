# Control mapping (NIST 800-53 Moderate / ASD STIG)

Status of OpenTrack 0.4.0 against the controls an application-level assessment covers. Controls
are cited by NIST 800-53 number. The ASD STIG requirements that implement them are named in
words, because STIG V-IDs change between releases: check each against the STIG release your
assessor uses. "Inherited" means the hosting environment provides the control.

Status: **Met**; **Partial** (met with a stated limit); **Open** (see the findings below);
**Inherited**.

| Control | Requirement | Status | How / where |
|---|---|---|---|
| AC-2 | Account management: create, change, turn off, remove; record who did it | Met | Settings → Users, `opentrack user …`; every change is in the decision log and the audit record |
| AC-2(3) | Turn off inactive accounts | Met | 35 days; checked at sign-in and every 10 minutes; exemptions for break-glass accounts |
| AC-3 | Enforce approved authorisations | Met | Roles `viewer < track_manager < admin`, checked on the server for every `/api/v1` route (`auth/policy.rs`); a source's secrets are hidden from everyone but admins (`ot_source::secrets`) |
| AC-6 | Least privilege | Met | Three roles; SAML can't grant admin unless **Allow admin** is on, and never signs in to a local account; the container drops every capability |
| AC-7 | Limit failed sign-ins | Met | 3 in 15 minutes lock the account for 15 minutes or until unlocked; plus a per-address rate limit |
| AC-8 | System use notification | Met | Notice-and-consent banner (Settings → Banners); declining signs the user out |
| AC-9 | Previous sign-in notification | Met | After sign-in: the last sign-in and the failed attempts since |
| AC-10 | Concurrent session control | Met | 3 sessions per account; the oldest ends |
| AC-11, AC-12 | Session lock and termination | Met | Idle timeout 15 minutes (admins 10), counted from the user's own activity; absolute lifetime `session_hours`; sessions kept on the server and revocable |
| AC-16 | Security attributes | Partial | Labels per source are carried on every track. A fused track takes the highest classification, the union of the restrictions and the intersection of the releasability lists. OpenTrack does not filter by clearance (a deliberate decision; enforcement is downstream) |
| AU-2, AU-3, AU-12 | Audit events and their content | Met | Sign-in success and failure (reason, address), sign-out, lockout, unlock, session end and timeout, password change, account turned off, every decision; actor, time, outcome, detail |
| AU-5 | Response to audit failure | Met | A sign-in that can't be recorded is refused |
| AU-6, AU-7 | Review, reduction and reports | Met | Settings → Audit (filters, CSV); `GET /api/v1/audit`; JSON logs for a SIEM |
| AU-9 | Protection of audit information | Met | Append-only table (triggers refuse UPDATE and DELETE), SHA-256 hash chain, `GET /api/v1/audit/verify`, chain head logged hourly |
| AU-11 | Retention | Met | Forever by default; a retention purge leaves a verifiable anchor and records itself |
| CM-6 | Configuration settings | Met (accepted risk) | STIG values are the defaults. `OT_AUTH=off` and per-feed `insecure_skip_verify` remain by the product owner's decision (F-4): the first warns every minute with a red banner; neither may be used in an accredited deployment (hardening checklist) |
| CM-7 | Least functionality | Met | One binary, one role per command; plugins sandboxed (WebAssembly, WASI grants) |
| IA-2 | Identification and authentication | Met | Local accounts, SAML, PKI client certificates, OpenStare trust, API tokens |
| IA-2(12) | PIV credentials | Met | Client certificates mapped to accounts by CN; revocation checked against CRLs (`OT_TLS_CLIENT_CRL`), failing closed on unknown status or a stale list, reloaded on change. No OCSP |
| IA-5(1) | Password-based authentication | Met | 15 characters, four classes, 8 changed, 5 remembered, 24 h minimum / 60 days maximum age, temporary passwords changed at first use, PBKDF2-HMAC-SHA256 (600,000 iterations) |
| IA-5(2) | PKI-based authentication | Met | Path validation to the configured CA and CRL status, as IA-2(12) |
| IA-7 | Cryptographic module authentication | Met | FIPS 140-3 modules only ([fips.md](fips.md)) |
| IA-8 | Non-organisational users | Inherited | Through the identity provider |
| SA-11, RA-5 | Developer testing, vulnerability scanning | Met | CI: tests, clippy `-D warnings`, `cargo deny` (RustSec), `npm audit`; image scanning at release |
| SC-8, SC-8(1) | Transmission confidentiality and integrity | Met | TLS for the UI and API, NATS, Redis and feeds, with mutual TLS available on each; HSTS |
| SC-13 | Cryptographic protection | Met | AWS-LC FIPS 3.0 and the OpenSSL 3.0.9 FIPS provider; `ring` banned in CI |
| SC-17 | PKI certificates | Inherited | Your CA issues the server and client certificates |
| SC-18 | Mobile code | Met | Content-Security-Policy: this origin only, no inline script. Inline *styles* are allowed (F-5) |
| SC-23 | Session authenticity | Met | Signed session tokens with a server-side session record; `HttpOnly`, `SameSite=Strict` and `Secure` cookies; no framing |
| SC-28 | Protection of information at rest | Inherited | Encrypt the `/data` volume and backups (the platform's disk encryption) |
| SI-2 | Flaw remediation | Met | Dependency advisories fail CI; rebuild on Debian security updates |
| SI-10 | Input validation | Met | Typed decoding for every source, size limits on bodies and messages; `unsafe` forbidden in OpenTrack's code |
| SR-3, SR-4 | Supply chain controls, provenance | Met | Lockfiles, pinned toolchain and base images, actions pinned by commit, SBOMs (CycloneDX); release images signed (cosign key) with SLSA provenance and SBOM attestations |

## Findings

| ID | Finding | Status |
|---|---|---|
| F-1 | A SAML sign-in whose email matched a **local** account signed in as that account, with its role (admin even with **Allow admin** off). | **Fixed:** SAML refuses any account it didn't make ("a local account has this email", audited) |
| F-2 | Viewers could read full source specifications, including inline credentials. | **Fixed:** for everyone but admins, passwords, tokens, header and metadata values, URL credentials and secret-named fields in transport messages read `••••••` in sources, revisions (the decision log carries no specs); `${env:…}` references stay |
| F-3 | The IdP entity ID, sign-in URL and certificate fields in Settings → Security looked editable, but sign-ons are checked against the pasted metadata alone. | **Fixed:** the three are read-only values read from the metadata on every save and read (what older settings stored is replaced); to change them, paste new metadata |
| F-4 | `OT_AUTH=off` and per-feed `insecure_skip_verify` exist. | **Accepted** by the product owner; excluded by the hardening checklist |
| F-5 | The CSP allows inline styles (`style-src 'unsafe-inline'`), which React, MapLibre and CodeMirror need. | Open (low) |
| F-6 | Client certificates were not checked for revocation. | **Fixed:** CRLs (`OT_TLS_CLIENT_CRL`), end-entity status, fail closed, hot reload. OCSP not supported |
| F-7 | Release images were not signed. | **Fixed:** `.github/workflows/release.yml` signs with the project key (no public log, the repository being private) and attaches SLSA provenance and SBOMs |
| F-8 | Turning an account off and on again revived its API tokens (a token issued in the same second as the turn-off survived it). | **Fixed:** turning an account off, by an admin or for inactivity, revokes its tokens for good |
| F-9 | `rust-toolchain.toml` followed `stable`. | **Fixed:** pinned to 1.98.1, here and in CI |
