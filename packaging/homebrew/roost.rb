# Source of truth for the navbytes/homebrew-tap tap's Formula/roost.rb —
# update THIS file per release (version + every sha256 from that release's
# SHA256SUMS.txt), then copy it into the tap repo. The values below are
# v0.1.4's, verified against the published checksums.
class Roost < Formula
  desc "Session-native terminal multiplexer for AI agent CLIs"
  homepage "https://github.com/navbytes/roost"
  version "0.1.4"
  license "MIT"

  # `url`/`sha256` aren't permitted inside on_macos/on_linux blocks (brew
  # style: FormulaAudit/ComponentsOrder) — plain OS/CPU conditionals instead.
  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-apple-darwin.tar.gz"
    sha256 "e9cbd6af8445c97134b3e852c456ca66afd66e412dcbfa9f54eb99f939fb65c8"
  elsif OS.mac?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-apple-darwin.tar.gz"
    sha256 "989924e043847a8d3e30dad30e942c4c03f194d37f0ea21634aae1b65ec3c312"
  elsif OS.linux? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-unknown-linux-gnu.tar.gz"
    sha256 "b331bb6dc4c9456a391e1b96e1ce8354c62fc87fd3ff50ac4fa27e45a81e74ca"
  elsif OS.linux?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "a0f788d0f6781eb74e54fa2adf3c6a43253801731524ff21852b30b816512e42"
  end

  def install
    bin.install "roost"
  end

  test do
    assert_match "roost spawn ADAPTER", shell_output("#{bin}/roost --help 2>&1")
  end
end
