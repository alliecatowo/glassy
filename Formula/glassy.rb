# Homebrew formula for glassy, a fast GPU-accelerated terminal emulator.
# This repo is its own tap, so:
#   brew tap alliecatowo/glassy https://github.com/alliecatowo/glassy
#   brew install glassy          # latest tagged release
#   brew install --HEAD glassy   # build from main
#
# NOTE: the url/sha256/version fields below are CI-managed. The release
# workflow's update-homebrew job substitutes real values and commits this file
# back to the default branch. Do not edit those lines by hand.
class Glassy < Formula
  desc "Fast, minimal GPU-accelerated terminal emulator written in Rust"
  homepage "https://github.com/alliecatowo/glassy"
  # CI-managed: the release workflow's update-homebrew job substitutes these
  # sentinels via sed and commits the result back to the default branch, so a
  # plain `brew install glassy` (after `brew tap alliecatowo/glassy`) gets the
  # latest tagged version. The url points at the RELEASE-UPLOADED source tarball
  # asset (not GitHub's auto-generated archive tarball) so url and sha256 refer
  # to the exact same bytes the workflow hashes — keep them in lockstep.
  url "https://github.com/alliecatowo/glassy/releases/download/GLASSY_VERSION/glassy-GLASSY_VERSION_PLAIN-src.tar.gz"
  sha256 "GLASSY_SHA256"
  version "GLASSY_VERSION_PLAIN"
  license "MIT"
  head "https://github.com/alliecatowo/glassy.git", branch: "main"

  depends_on "rust" => :build
  depends_on "pkg-config" => :build
  depends_on "fontconfig"

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
