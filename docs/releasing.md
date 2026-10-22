# Releasing

Seven crates go up separately, and the order is forced. Everything here is
`cargo`; there is no release tooling to learn.

## The order

```
doclite-core
doclite-derive
doclite-store
doclite-testkit
doclite-memory
doclite-sqlite
doclite
```

A workspace dependency is written as a path and a version. The path is what a
local build uses and crates.io throws it away, so an uploaded crate resolves
against the index alone. Publish `doclite-store` before `doclite-core` and the
upload is refused with `no matching package named doclite-core found`, because
by then the path is gone and the index has nothing to put in its place.

`doclite-derive` depends on nothing else in the workspace and could go first.
It is second because that is the list read top-down, and a fixed order is worth
more than a shorter one.

## Before anything is uploaded

```sh
cargo fmt --all --check
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo +1.82 check --workspace --all-targets --all-features
```

The last one is the `rust-version` every manifest claims. It is a promise to
callers rather than a note, so it is checked rather than assumed.

`.github/workflows/ci.yml` runs all six on every push and pull request, so on a
green branch this is a second opinion rather than the first. It runs the tests
on Linux and macOS, because the SQLite engine compiles a bundled C library and
touches the filesystem and neither is the same on both.

Then check that `CHANGELOG.md`'s top heading carries the version being
published and the date it is going out on. A release whose heading still names
the last one is one nobody can tell apart from it, and a date earlier than the
upload is a claim the file cannot support, so move it rather than leave it.

Then, per crate in the order above:

```sh
cargo publish --dry-run -p <crate>
cargo publish -p <crate>
```

## What to expect between crates

The index takes a moment to catch up after an upload. The next crate's dry-run
can fail on a dependency that was published seconds earlier, and that is the
index lagging rather than anything being wrong. Wait and run it again.

Only `doclite-core` and `doclite-derive` can be dry-run before the rest of a
release is uploaded. This is true of every release and not only the first: a
dry-run builds the package with the path stripped, so the other five resolve the
version the workspace pins against an index that does not hold it yet, and fail
for the same reason a real publish out of order would.

`cargo publish --dry-run --no-verify -p <crate>` does work mid-release. It
packages the crate and checks the manifest without compiling it, which is the
cheapest thing that answers whether a manifest is publishable at all.

`cargo package --list -p <crate>` is the other thing that can be checked ahead
of time, and it is what confirms `LICENSE` and `README.md` land in every
`.crate`. CI's `package` job runs exactly that over all seven, so this one is
answered on every push rather than remembered at release time.

## What a dry-run does not check

**Category slugs.** crates.io refuses a category it does not know, and cargo
never looks. Check every `categories` entry against
<https://crates.io/categories> before the first upload of a release.

**Dev-dependencies with a path and no version.** Cargo strips them from the
published manifest. That is why `doclite` can dev-depend on `doclite-memory`
without a cycle in the publish order, and also why the doctests that reach for
`doclite_memory` do not run on docs.rs.

## Tags

A release is tagged `v<version>` at the commit that was uploaded, annotated and
unsigned. The tag is pushed after the last crate lands, so one never names a
tree that failed to publish.

## Version numbers

Every crate shares one version through `[workspace.package]`, and they are
released together. A change to `doclite-core` alone still moves all seven,
because a version pair that has never been built together is a support question
nobody wants.
