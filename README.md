# Nyx

> A fast, reliable, cross-platform file-transfer client written in Rust — built the way Zed is built.

Nyx is a desktop **SFTP/FTP/FTPS** client focused on reliability, simplicity and
performance rather than feature-completeness. Create connection profiles, browse
remote directories, and transfer files — with a polished native UI rendered with
[GPUI](https://github.com/zed-industries/zed) (Zed's GPU-accelerated framework),
not a web stack.

**Using the app?** See the [user guide](docs/user/README.md) —
[install](docs/user/install.md), [getting started](docs/user/getting-started.md),
and [troubleshooting](docs/user/troubleshooting.md).

## Architecture

GPUI runs on the main thread with its own executor; a dedicated **Tokio** backend
thread owns all connections and the transfer queue, communicating with the UI
over channels. Crates:

| Crate | Role |
|---|---|
| `nyx` | GPUI app binary: window, root view, app state |
| `nyx-ui` | In-house component library + theme (extract → **Flint**). |
| `nyx-core` | Shared types: errors, `RemoteEntry`, transfer model |
| `nyx-service` | Tokio backend thread + command/event channels |
| `nyx-protocol` | `RemoteClient` trait + `SftpClient` (V1) |
| `nyx-transfer` | Queue, concurrency, progress, cancellation |
| `nyx-profile` | Profile store |
| `nyx-keyring` | OS-keychain credentials |

## Build & run

Requires Rust (see [`rust-toolchain.toml`](rust-toolchain.toml)), **full Xcode**
(GPUI's Metal shaders are compiled at build time with the `metal` tool, which
ships only in `Xcode.app` — the Command Line Tools alone are not enough), and
`cmake` (`brew install cmake`). One-time macOS setup:

```sh
sudo xcodebuild -license accept
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

```sh
cargo run -p nyx                              # open the app window
cargo run -p nyx-ui --example gallery         # the component gallery ("storybook")
cargo build                                   # build the whole workspace
cargo test                                    # run tests
cargo clippy --all-targets -- -D warnings     # lint
cargo fmt                                      # format
```

> The **first build is long** — it compiles the whole GPUI dependency tree
> (several minutes). GPUI is pinned to a specific Zed git revision for
> reproducibility; don't bump it casually.

## Packaging a macOS release

[`scripts/bundle-mac.sh`](scripts/bundle-mac.sh) builds a `Nyx.app` bundle and a
`.dmg` installer. It needs no extra tooling — only `sips`, `iconutil` and
`hdiutil`, which ship with macOS. The app icon is generated from
[`assets/nyx.png`](assets/nyx.png), and the fonts/icons are already embedded in
the binary (rust-embed), so the bundle is just the executable + `Info.plist` +
icon.

```sh
just bundle-mac              # build for the host arch
just bundle-mac --universal  # universal (arm64 + x86_64) binary, for sharing
```

Outputs land under `target/macos/` (gitignored): `Nyx.app` and
`Nyx-<version>.dmg`. The version is read from `[workspace.package]` in
`Cargo.toml`.

> The bundle is **ad-hoc signed** — it runs on the build machine, but anyone
> else who downloads the `.dmg` will hit Gatekeeper. Distributing it to others
> requires Developer ID signing + notarization (`codesign --sign "Developer ID
> Application: …"`, then `xcrun notarytool submit` + `xcrun stapler staple`),
> which the script intentionally leaves out.

## License

[GPL-3.0-or-later](LICENSE) © 2026 vojir-mikulas

Nyx builds on [GPUI](https://github.com/zed-industries/zed) (Apache-2.0), whose
dependency tree includes GPL-3.0-or-later utility crates from the Zed repo
(`zlog`, `ztracing`). The distributed Nyx binary is therefore licensed under the
GPL-3.0-or-later; see [`NOTICE`](NOTICE) for the third-party attributions.
