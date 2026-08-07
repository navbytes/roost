# Source of truth for the navbytes/homebrew-roost tap's Formula/roost.rb —
# update THIS file per release (version + every sha256 from that release's
# SHA256SUMS.txt), then copy it into the tap repo. The values below are
# v0.1.2's, verified against the published checksums.
class Roost < Formula
  desc "Session-native terminal multiplexer for AI agent CLIs"
  homepage "https://github.com/navbytes/roost"
  version "0.1.2"
  license "MIT"

  # `url`/`sha256` aren't permitted inside on_macos/on_linux blocks (brew
  # style: FormulaAudit/ComponentsOrder) — plain OS/CPU conditionals instead.
  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-apple-darwin.tar.gz"
    sha256 "b15bf4b250c92232c810025cbadcb6e4e8b6c65bcabeac5ee280de92e021e74e"
  elsif OS.mac?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-apple-darwin.tar.gz"
    sha256 "de751bd83ca365767657f15398c335ad9d95d03cd198670f575a1835a0db4ebc"
  elsif OS.linux? && Hardware::CPU.arm?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-aarch64-unknown-linux-gnu.tar.gz"
    sha256 "a232d2f9f24db69ec14dd75fd5a78e83f0baea911658f3eefb4f41a44f80c318"
  elsif OS.linux?
    url "https://github.com/navbytes/roost/releases/download/v#{version}/roost-#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "2afdffc111dc98767905daf350622ff276f5f7e769c491e9de18c86f66d92801"
  end

  def install
    bin.install "roost"
  end

  test do
    assert_match "roost spawn ADAPTER", shell_output("#{bin}/roost --help 2>&1")
  end
end
