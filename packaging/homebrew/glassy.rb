# Homebrew formula for glassy, a fast GPU-accelerated terminal emulator.
#
# Builds from the tagged v0.1.0 source tarball on GitHub.
#
# To install from this formula directly:
#   brew install --build-from-source ./packaging/homebrew/glassy.rb
class Glassy < Formula
  desc "Fast, minimal GPU-accelerated terminal emulator written in Rust"
  homepage "https://github.com/alliecatowo/glassy"
  url "https://github.com/alliecatowo/glassy/archive/refs/tags/v0.1.0.tar.gz"
  # TODO: replace with the real sha256 of the v0.1.0 source tarball:
  #   curl -L https://github.com/alliecatowo/glassy/archive/refs/tags/v0.1.0.tar.gz | shasum -a 256
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  head "https://github.com/alliecatowo/glassy.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "glassy", shell_output("#{bin}/glassy --version 2>&1", 0)
  end
end
