# Supply chain

This covers what goes into an OpenTrack build and how that is checked.

## Pinned inputs

| Input | How it is pinned |
|---|---|
| Rust crates | `Cargo.lock`, built with `--locked` |
| UI packages | `ui/package-lock.json`, installed with `npm ci`; the stareSDK tarball is vendored in `ui/vendor` |
| Forked crates | `third_party/samael` (SAML) and `third_party/rumqttc` (MQTT; FIPS patch). Each has a note on what differs from upstream |
| Base images | Pinned by digest in the `Dockerfile` |
| Go (FIPS module build) | Version and SHA-256 checked in the `Dockerfile` |
| OpenSSL FIPS provider | Release 3.0.9 source, SHA-256 checked in the `Dockerfile` |
| CI actions | Pinned by commit SHA in `.github/workflows/*.yml` |

**Moving a base image:** run
`docker buildx imagetools inspect <image>:<tag>` and replace the digest after
the `@`. Rebuild and run the tests, then note the change in CHANGELOG.

## Checks in CI

- **`cargo deny check`** (`deny.toml`):
  - known vulnerabilities (RustSec);
  - licences against an allow-list;
  - crates banned for FIPS (`ring`, `openssl-src`, `native-tls`);
  - dependencies only from crates.io.
- **`npm audit --omit=dev --audit-level=high`** on the UI's runtime
  dependencies.
- **SBOMs**, CycloneDX JSON, attached to every CI run as the `sbom` artifact:
  - `opentrack.cdx.json`, the Rust binary's dependency tree (`cargo cyclonedx`);
  - `opentrack-ui.cdx.json`, the UI bundle's (`npm sbom --sbom-format cyclonedx`).

## Making SBOMs locally

```sh
cargo install --locked cargo-cyclonedx cargo-deny
(cd ui && npm ci)                  # npm 10 or later (Node 22)
scripts/security/sbom.py           # target/sbom/opentrack.cdx.json, opentrack-ui.cdx.json
cargo deny check
```

`cargo cyclonedx` reads `cargo metadata`, which merges the features of every
target. That makes it list crates the Linux build never compiles, such as
`ring` through a WebAssembly-only edge of `quinn`. The script keeps only the
crates `cargo tree` reports as compiled into the binary. It records the ones
it dropped in the SBOM's metadata (`opentrack:filtered`), and it fails if a
banned crate is ever built.

## Releases

Publishing a GitHub release runs `.github/workflows/release.yml`, which:

1. Builds the image with BuildKit's SLSA provenance (`mode=max`) and SBOM attestations attached.
2. Pushes it to the private package `ghcr.io/phornstein/opentrack:<tag>`.
3. Signs its digest with the project's cosign key. The signature goes to no public transparency
   log, because the repository is private and a public entry would name it;
   `.github/cosign/signing-config.json` lists no log. The workflow then verifies the signature.
4. Attaches the two SBOMs and `image-digest.txt` to the release.

**Verify an image:**

```sh
cosign verify --key cosign.pub --insecure-ignore-tlog=true ghcr.io/phornstein/opentrack@<digest>
docker buildx imagetools inspect ghcr.io/phornstein/opentrack@<digest> --format '{{ json .Provenance }}'
```

**The key:**
- The public key is `cosign.pub` in the repository.
- The private key and its password are the repository secrets `COSIGN_PRIVATE_KEY` and
  `COSIGN_PASSWORD`. The maintainer keeps a backup readable only by them, outside the repository.
- **Rotate it:** `cosign generate-key-pair`, replace both secrets, commit the new `cosign.pub`,
  and re-sign the images still supported.

The operating-system packages in the image come from Debian bookworm. Scan each release image with
the scanner your accreditation uses (for example `grype` or `trivy`) and keep the report with the
release.

The Rust toolchain is pinned in `rust-toolchain.toml` (and in CI), so every build of a commit uses
the same compiler. Move it deliberately and run the tests when you do.
