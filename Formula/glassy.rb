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
  url "https://github.com/alliecatowo/glassy/archive/refs/tags/GLASSY_VERSION.tar.gz"
  sha256 "GLASSY_SHA256"
  version "GLASSY_VERSION_PLAIN"
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
