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
| AC-3 | Enforce approved authorisations | Partial | Roles `viewer < track_manager < admin`, checked on the server for every `/api/v1` route (`auth/policy.rs`). Finding F-2 |
| AC-6 | Least privilege | Met | Three roles; SAML can't grant admin unless **Allow admin** is on (see F-1); the container drops every capability |
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
| CM-6 | Configuration settings | Partial | STIG values are the defaults. `OT_AUTH=off` and per-feed `insecure_skip_verify` still exist (F-4) |
| CM-7 | Least functionality | Met | One binary, one role per command; plugins sandboxed (WebAssembly, WASI grants) |
| IA-2 | Identification and authentication | Met | Local accounts, SAML, PKI client certificates, OpenStare trust, API tokens |
| IA-2(12) | PIV credentials | Partial | Client certificates are mapped to accounts by CN; there is no OCSP or CRL checking (F-6) |
| IA-5(1) | Password-based authentication | Met | 15 characters, four classes, 8 changed, 5 remembered, 24 h minimum / 60 days maximum age, temporary passwords changed at first use, PBKDF2-HMAC-SHA256 (600,000 iterations) |
| IA-5(2) | PKI-based authentication | Partial | As IA-2(12) |
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
| SR-3, SR-4 | Supply chain controls, provenance | Partial | Lockfiles, pinned base images, actions pinned by commit, SBOMs (CycloneDX). Images are not yet signed (F-7) |

## Open findings

Each finding is open until the product owner decides on it.

| ID | Finding | Risk | Proposed fix |
|---|---|---|---|
| F-1 | A SAML sign-in whose email matches a **local** account signs in as that account, **with its role**. It can be admin even when **Allow admin** is off, or when the role mapping gives the user nothing. | High | Refuse SAML for local accounts, or apply the mapping and the **Allow admin** cap to them too |
| F-2 | Viewers can read full source specifications, including any credential written inline instead of `${env:NAME}`. | Medium | Redact secret fields for viewers, or require `${env:…}` for credentials |
| F-3 | The IdP entity ID, sign-in URL and certificate fields in Settings → Security are display-only: sign-ons are checked against the pasted metadata. | Low (misleading configuration) | Make the fields read-only, or build the service provider from them |
| F-4 | `OT_AUTH=off` and per-feed `insecure_skip_verify` exist. | Medium if used | Refuse both unless an explicit laboratory flag is set |
| F-5 | The CSP allows inline styles (`style-src 'unsafe-inline'`), which React, MapLibre and CodeMirror need. | Low | Nonce- or hash-based styles across stareSDK |
| F-6 | Client certificates are not checked for revocation (no OCSP or CRL). | Medium where PKI sign-in is used | CRL files or OCSP at the TLS layer |
| F-7 | Release images are not signed and carry no provenance attestation. | Low | cosign signatures and SLSA provenance in the release workflow |
| F-8 | Turning an account off and on again brings its API tokens back to life. A password reset revokes them. | Low | Revoke tokens when an account is turned off |
| F-9 | `rust-toolchain.toml` follows `stable`, so two builds of one commit can use different compilers. | Low | Pin the toolchain version |
