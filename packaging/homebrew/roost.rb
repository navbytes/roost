# Formula for the eventual navbytes/homebrew-roost tap (the tap repo is not
# created yet — see ../README.md). `version` and every `sha256` are filled in
# per release from that release's SHA256SUMS.txt, not by Homebrew itself;
# the values below are v0.1.0's, verified against the published checksums.
class Roost < Formula
  desc "Session-native terminal multiplexer for AI agent CLIs"
  homepage "https://github.com/navbytes/roost"
  version "0.1.0"
  license "MIT"

  # `url`/`sha256` aren't permitted inside on_macos/on_linux blocks (brew
  # style: FormulaAudit/ComponentsOrder) — plain OS/CPU conditionals instead.
  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-apple-darwin.tar.gz"
    sha256 "d14b3d337bae8e5be7434600e039bdec72ecbe65238a6c168cbbb00f3b45201f"
  elsif OS.mac?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-apple-darwin.tar.gz"
    sha256 "7f32d0c7b3a110764a59e0241d375b1cd41af1d96bfc8a8d1133323d4a43eaca"
  elsif OS.linux? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-unknown-linux-gnu.tar.gz"
    sha256 "0a2e3ddcaa3aa4f6ca9a052a5cd287d67f20ab5832ac0e8c7a514ce3135ce43d"
  elsif OS.linux?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "fe8a65d40de8c4fe7f84e92004c2b1508f8b72ada4e04790210b3c7ac124c3f8"
  end

  def install
    bin.install "roost"
  end

  test do
    assert_match "roost spawn ADAPTER", shell_output("#{bin}/roost --help 2>&1")
  end
end
