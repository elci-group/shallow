# Shallowgrave

An immutable, indexed historical archive of software releases and their
executable artefacts.

> Deprecation is not deletion. A software version that has ceased to be
> active remains an artefact of the application's history.

Shallowgrave is a local, content-addressable archive with manual release
commands, multi-platform manifests, integrity auditing, GitHub Release sync,
and daemon-based tracking of Cargo project builds.

## Layout

- `shallowgrave-core` — library: manifest schema, hashing, git provenance,
  the `ArtifactStore` trait, and a `LocalStore` implementation.
- `shallowgrave-cli` — the `shallowgrave` binary.

## Usage

```bash
export SHALLOWGRAVE_HOME=~/.shallowgrave   # optional, this is the default

shallowgrave archive ./target/release/uni --app uni --version 1.4.2 \
    --target x86_64-unknown-linux-gnu --profile release

shallowgrave find uni
shallowgrave inspect uni@1.4.2
shallowgrave verify uni@1.4.2
shallowgrave resurrect uni@1.4.2 --output ./uni
```

## Daemon-based tracking

Register a Cargo project once, then run the daemon under your preferred service
manager. Each scan reads the current package name and version from `Cargo.toml`
and archives the release only when its app, version, or binary digest changed.
One broken or not-yet-built project is recorded as an error without preventing
other projects from being archived. Changes in source provenance, toolchain,
target, profile, and features are tracked too, even if the resulting bytes are
identical. A PID guard prevents two daemon loops from racing over the same
archive and automatically recovers a stale PID after an unclean shutdown.

```bash
# Binary defaults to target/release/<package-name>.
shallowgrave track add /path/to/project

# Inspect persistent state, including the last archive or error.
shallowgrave track list

# Foreground service loop (well suited to systemd, launchd, or supervisord).
shallowgrave daemon --interval 10

# A single scan for CI, cron, or a systemd timer. It exits nonzero on errors.
shallowgrave daemon --once --json

# Removing a watch never removes archived releases or objects.
shallowgrave track remove /path/to/project
```

Use `--binary path/to/file`, `--app name`, `--target triple`, `--profile name`,
or repeated `--feature name` arguments with `track add` when the inferred
defaults do not match the build. Tracking configuration and last-observed state
are atomically persisted at `$SHALLOWGRAVE_HOME/tracking.json`.

## Archive format

```text
$SHALLOWGRAVE_HOME/
├── objects/<sha256[0:2]>/<sha256>   # content-addressable, deduplicated
├── manifests/<app>/<version>.json   # shallowgrave/v2 manifest
└── tracking.json                    # daemon registrations and last results
```

Manifests are the authoritative index; artefact bytes are looked up by
digest. Nothing is ever mutated in place — resurrection re-verifies the
digest of the restored file before reporting success, and no artefact is
ever executed automatically.

## Testing

```bash
cargo test --workspace
```

`shallowgrave-core/tests/recovery.rs` exercises the acceptance-criteria
workflow from the directive directly: archive → delete local build
artefacts → resurrect → verify digest match.
# shallow
