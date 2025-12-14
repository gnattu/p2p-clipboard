## 0.6.0

- Sync upstream v0.6.0

## 0.4.0

- Fork from upstream, tweaked for p2p-clipboard
- Added runtime detection for cipher-suite selection. Prefer AES-GCM on CPUs with aes acceleration, otherwise prefer ChaCha20-Poly1305
- Added extra authentication through a user-defined PSK. When verifying the signature defined in the certificate extension, the PSK is required to unroll the signature. 

## 0.3.0

- Migrate to `{In,Out}boundConnectionUpgrade` traits.
  See [PR 4695](https://github.com/libp2p/rust-libp2p/pull/4695).

## 0.2.1

- Switch from webpki to rustls-webpki.
  This is a part of the resolution of the [RUSTSEC-2023-0052].
  See [PR 4381].

[PR 4381]: https://github.com/libp2p/rust-libp2p/pull/4381
[RUSTSEC-2023-0052]: https://rustsec.org/advisories/RUSTSEC-2023-0052.html

## 0.2.0

- Raise MSRV to 1.65.
  See [PR 3715].

[PR 3715]: https://github.com/libp2p/rust-libp2p/pull/3715

## 0.1.0

- Promote to `v0.1.0`.

## 0.1.0-alpha.2

- Update to `libp2p-core` `v0.39.0`.

## 0.1.0-alpha

Initial release.
