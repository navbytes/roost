# Source of truth for the navbytes/homebrew-tap tap's Formula/roost.rb —
# update THIS file per release (version + every sha256 from that release's
# SHA256SUMS.txt), then copy it into the tap repo. The values below are
# v0.1.5's, verified against the published checksums.
class Roost < Formula
  desc "Session-native terminal multiplexer for AI agent CLIs"
  homepage "https://github.com/navbytes/roost"
  version "0.1.5"
  license "MIT"

  # `url`/`sha256` aren't permitted inside on_macos/on_linux blocks (brew
  # style: FormulaAudit/ComponentsOrder) — plain OS/CPU conditionals instead.
  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-apple-darwin.tar.gz"
    sha256 "19f5fb4347564ddfc4e6871542f64e41f331a814e6c89e1e4d08411be33fc353"
  elsif OS.mac?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-apple-darwin.tar.gz"
    sha256 "3e68c9c4e12ca3b46f0a6e8376e6646c3121a109985657d0d8aed03464e5118d"
  elsif OS.linux? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-unknown-linux-gnu.tar.gz"
    sha256 "1cc7345561926ca285b5c49bf543d613e6cb602c90db0b4b75c27901dcdc8e98"
  elsif OS.linux?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "4eb8ebe985c44fd1e80e3a29ca91ba78563fd9e6785b6010c7181f8f0c370b25"
  end

  def install
    bin.install "roost"
  end

  test do
    assert_match "roost spawn ADAPTER", shell_output("#{bin}/roost --help 2>&1")
  end
end
