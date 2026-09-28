# rumqttc 0.25.1 (patched)

Published rumqttc 0.25.1 with one change in `Cargo.toml`: `rustls-webpki`
0.102 is taken without default features, so its `ring` backend is not built.
rumqttc uses only webpki's error type; TLS goes through the rustls
configuration OpenTrack passes in (AWS-LC FIPS provider). Remove this copy
when an upstream release drops the old webpki dependency.
