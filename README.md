# Nyx

> A fast, reliable, cross-platform file-transfer client written in Rust — built the way Zed is built.

Nyx is a desktop **SFTP/FTP/FTPS** client focused on reliability, simplicity and
performance rather than feature-completeness. Create connection profiles, browse
remote directories, and transfer files — with a polished native UI rendered with
[GPUI](https://github.com/zed-industries/zed) (Zed's GPU-accelerated framework),
not a web stack.

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

## License

[Apache-2.0](LICENSE) © 2026 vojir-mikulas
