# Source of truth for the navbytes/homebrew-roost tap's Formula/roost.rb —
# update THIS file per release (version + every sha256 from that release's
# SHA256SUMS.txt), then copy it into the tap repo. The values below are
# v0.1.3's, verified against the published checksums.
class Roost < Formula
  desc "Session-native terminal multiplexer for AI agent CLIs"
  homepage "https://github.com/navbytes/roost"
  version "0.1.3"
  license "MIT"

  # `url`/`sha256` aren't permitted inside on_macos/on_linux blocks (brew
  # style: FormulaAudit/ComponentsOrder) — plain OS/CPU conditionals instead.
  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-apple-darwin.tar.gz"
    sha256 "ceb7787cdc6f77de9cc42523b9ab1a3a9ac2ec196c602aa46066f7cf844363e5"
  elsif OS.mac?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-apple-darwin.tar.gz"
    sha256 "a8edb2c086489f1c0e06f7992aa6786d232fc69fccc64a74e41d9774d4bc6701"
  elsif OS.linux? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-unknown-linux-gnu.tar.gz"
    sha256 "c7a9bea81ac9a7e6d4d73a48a355570e30e55318dbf3c9443e8517f75eb27204"
  elsif OS.linux?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "bee6ff4aae59485a529b530eda924c07de8dcd1fc64cb4dca8aa4d370a0b3abe"
  end

  def install
    bin.install "roost"
  end

  test do
    assert_match "roost spawn ADAPTER", shell_output("#{bin}/roost --help 2>&1")
  end
end
