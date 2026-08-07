# Source of truth for the navbytes/homebrew-roost tap's Formula/roost.rb —
# update THIS file per release (version + every sha256 from that release's
# SHA256SUMS.txt), then copy it into the tap repo. The values below are
# v0.1.1's, verified against the published checksums.
class Roost < Formula
  desc "Session-native terminal multiplexer for AI agent CLIs"
  homepage "https://github.com/navbytes/roost"
  version "0.1.1"
  license "MIT"

  # `url`/`sha256` aren't permitted inside on_macos/on_linux blocks (brew
  # style: FormulaAudit/ComponentsOrder) — plain OS/CPU conditionals instead.
  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-apple-darwin.tar.gz"
    sha256 "1a6756ace0fb2e00bea3eb8bc424a47eeed5607612024b2ac2d74a29a644b30e"
  elsif OS.mac?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-apple-darwin.tar.gz"
    sha256 "2f378035fe489877a3703d174ff114265340105929076e6e83be995ef7436d66"
  elsif OS.linux? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-unknown-linux-gnu.tar.gz"
    sha256 "135995d975b00ac9298d6d41f1a9d57cbae7bfc864dabe42f7a6db914830fb9c"
  elsif OS.linux?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "ab94cfa09b150bce34efb39c60a7b7fb494ec0be5ce915fe3625b70d6fee2171"
  end

  def install
    bin.install "roost"
  end

  test do
    assert_match "roost spawn ADAPTER", shell_output("#{bin}/roost --help 2>&1")
  end
end
