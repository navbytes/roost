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

## The Homebrew formula (automated)

The release workflow keeps the tap current — no manual bump. After the
GitHub Release is published, the `release` job runs
[`scripts/update-homebrew-formula.sh`](../scripts/update-homebrew-formula.sh),
which renders `Formula/roost.rb` (version + all four sha256s parsed from
`dist/SHA256SUMS.txt`) and pushes it to
[`navbytes/homebrew-tap`](https://github.com/navbytes/homebrew-tap). The
script is the formula's source of truth — there is no checked-in `.rb`
template; edit the script's heredoc to change the formula.

The step needs one secret in this repo's Actions settings:

- **`HOMEBREW_TAP_TOKEN`** — a fine-grained PAT with **Contents: Read and
  write** on **only** `navbytes/homebrew-tap` (GitHub → Settings → Developer
  settings → Fine-grained tokens). It's the same secret name, scope, and
  target tap that vee's release workflow uses, so the same token value works
  for both repos. When the secret is absent the step is skipped and the
  release still succeeds — the tap just doesn't update (fork-safe).

Dry-run locally (renders to stdout, touches nothing):

```sh
RENDER_ONLY=1 RELEASE_TAG=v0.1.4 SUMS_FILE=path/to/SHA256SUMS.txt \
  scripts/update-homebrew-formula.sh
```

## The tap

`brew install navbytes/tap/roost` serves the formula from
[`navbytes/homebrew-tap`](https://github.com/navbytes/homebrew-tap) — the
general navbytes tap (this formula plus the vee and nt casks). The earlier
dedicated `navbytes/homebrew-roost` tap holds only a `tap_migrations.json`
redirect pointing there; leave that repo up so pre-move installs migrate
automatically on `brew update` + `brew upgrade`.
