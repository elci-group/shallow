# Shallowgrave

An immutable, indexed historical archive of software releases and their
executable artefacts.

> Deprecation is not deletion. A software version that has ceased to be
> active remains an artefact of the application's history.

This is the v1 MVP described in the project directive (§45): a local,
content-addressable archive with archive / find / inspect / verify /
resurrect. It does not yet implement GitHub Release publication, UNI /
Kaptaind / Padagonia integration, or multi-platform artefacts — those are
later phases.

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

## Archive format

```text
$SHALLOWGRAVE_HOME/
├── objects/<sha256[0:2]>/<sha256>   # content-addressable, deduplicated
└── manifests/<app>/<version>.json   # shallowgrave/v1 manifest
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
