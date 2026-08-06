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

`packaging/homebrew/roost.rb` is a template — it isn't wired into a tap yet
(see below), so nothing installs it automatically. After a release, a
maintainer updates it by hand:

1. Set `version` to the released `X.Y.Z` (no `v` prefix).
2. Download `SHA256SUMS.txt` from the release and copy each matching line's
   hash into the corresponding `REPLACE_ON_RELEASE` in the formula:
   - `roost-<version>-aarch64-apple-darwin.tar.gz` → macOS arm64
   - `roost-<version>-x86_64-apple-darwin.tar.gz` → macOS x64
   - `roost-<version>-x86_64-unknown-linux-gnu.tar.gz` → Linux x64

   `grep aarch64-apple-darwin SHA256SUMS.txt` (etc.) finds the right line.

## The tap

Installing via `brew install navbytes/roost/roost` needs the formula to live
in a `navbytes/homebrew-roost` repo — that's its eventual home, but creating
it is deliberately deferred until there's a real release to point it at.
Until then, this formula is a template to copy into that repo later, not
something you can `brew install` today.
