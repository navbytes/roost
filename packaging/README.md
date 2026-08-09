# Packaging

## Cutting a release

A maintainer cuts a release by tagging and pushing:

```sh
git tag vX.Y.Z
git push --tags
```

The tag must match the `version` in `Cargo.toml` — `.github/workflows/release.yml`
checks this and fails the build if they've drifted apart. Pushing the tag
triggers that workflow, which builds all four target binaries, packages each
as `roost-<version>-<target>.tar.gz` (binary + LICENSE + README.md), and
publishes a GitHub Release with the tarballs plus a combined `SHA256SUMS.txt`
attached.

## Updating the Homebrew formula

`packaging/homebrew/roost.rb` is the source of truth for the tap's formula.
After a release, a maintainer updates it by hand:

1. Set `version` to the released `X.Y.Z` (no `v` prefix).
2. Download `SHA256SUMS.txt` from the release and copy each matching line's
   hash into the corresponding `sha256` in the formula:
   - `roost-<version>-aarch64-apple-darwin.tar.gz` → macOS arm64
   - `roost-<version>-x86_64-apple-darwin.tar.gz` → macOS x64
   - `roost-<version>-aarch64-unknown-linux-gnu.tar.gz` → Linux arm64
   - `roost-<version>-x86_64-unknown-linux-gnu.tar.gz` → Linux x64

   `grep aarch64-apple-darwin SHA256SUMS.txt` (etc.) finds the right line.
3. Copy the updated formula into
   [`navbytes/homebrew-tap`](https://github.com/navbytes/homebrew-tap) as
   `Formula/roost.rb`, swapping the header comment for the tap copy's
   "copied from navbytes/roost" note.

## The tap

`brew install navbytes/tap/roost` serves the formula from
[`navbytes/homebrew-tap`](https://github.com/navbytes/homebrew-tap) — the
general navbytes tap (this formula plus the vee and nt casks). The earlier
dedicated `navbytes/homebrew-roost` tap holds only a `tap_migrations.json`
redirect pointing there; leave that repo up so pre-move installs migrate
automatically on `brew update` + `brew upgrade`.
