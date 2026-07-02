# Homebrew formula for glassy, a fast GPU-accelerated terminal emulator.
#
# To install from this formula directly (build from source):
#   brew install --build-from-source ./packaging/homebrew/glassy.rb
#
# NOTE: the url/sha256/version fields below are CI-managed.  The release
# workflow substitutes real values before opening a PR against a tap repo.
# Do not edit those lines by hand.
class Glassy < Formula
  desc "Fast, minimal GPU-accelerated terminal emulator written in Rust"
  homepage "https://github.com/alliecatowo/glassy"
  # CI fills these in via sed before the PR to the tap; keep the sentinel values.
  # url points at the release-uploaded source tarball asset so it matches the
  # sha256 the workflow computes (the GitHub auto-archive tarball has a different
  # hash). Unlike Formula/glassy.rb (this repo's own tap, which installs a
  # prebuilt binary for speed), this formula deliberately always builds from
  # source — that's the point of it: a homebrew-core submission requires a
  # build-from-source formula, since core disallows vendored binaries.
  url "https://github.com/alliecatowo/glassy/releases/download/v0.2.0-rc2/glassy-0.2.0-rc2-src.tar.gz"
  sha256 "971b314d888e172029b1f55213d02a23b4a4d045fea8254c460e2a0d4cf48970"
  version "0.2.0-rc2"
  license "MIT"
  head "https://github.com/alliecatowo/glassy.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  # Install man page if present.
  def post_install
    man1.mkpath
    man1.install "extra/glassy.1" if File.exist?("extra/glassy.1")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/glassy --version 2>&1", 0)
  end
end
