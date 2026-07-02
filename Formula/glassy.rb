# Homebrew formula for glassy, a fast GPU-accelerated terminal emulator.
# This repo is its own tap, so:
#   brew tap alliecatowo/glassy https://github.com/alliecatowo/glassy
#   brew install glassy          # latest tagged release
#   brew install --HEAD glassy   # build from main (requires Rust)
#
# NOTE: the version/sha256 fields below are CI-managed. The release
# workflow's update-homebrew job substitutes real values and commits this file
# back to the default branch. Do not edit those lines by hand.
#
# On macOS, stable installs fetch the prebuilt per-arch binary that
# build-macos already uploads as a release asset (glassy-aarch64-macos /
# glassy-x86_64-macos) instead of compiling from source — `cargo install`
# here took several minutes (lto = "fat" + codegen-units = 1 in Cargo.toml),
# while downloading an already-built binary takes seconds. Linux stable
# installs still build from the top-level source tarball (no prebuilt Linux
# binary asset is wired up here yet). `--HEAD` always builds from source on
# any OS, since there's no prebuilt asset for an arbitrary main commit.
class Glassy < Formula
  desc "Fast, minimal GPU-accelerated terminal emulator written in Rust"
  homepage "https://github.com/alliecatowo/glassy"
  url "https://github.com/alliecatowo/glassy/releases/download/v0.4.1/glassy-0.4.1-src.tar.gz"
  version "0.4.1"
  sha256 "9f24000ab7ef46fd6c2fcc44e7d1173b92b2617d65ce5d0c258604f9a6d9ea9e"
  license "MIT"

  head do
    url "https://github.com/alliecatowo/glassy.git", branch: "main"
    depends_on "pkg-config" => :build
    depends_on "rust" => :build
  end

  # macOS-only override: swap the default source tarball for a prebuilt
  # per-arch binary. Homebrew resolves on_arm/on_intel against the host's
  # arch, so `brew install glassy` on macOS never touches the url/sha256 above.
  on_macos do
    on_arm do
      url "https://github.com/alliecatowo/glassy/releases/download/v0.4.1/glassy-aarch64-macos"
      sha256 "73ec05b35382e8330fa8c8259a299813e095241c89a98038ba170ad262e6bd91"
    end
    on_intel do
      url "https://github.com/alliecatowo/glassy/releases/download/v0.4.1/glassy-x86_64-macos"
      sha256 "c9c43bea96aeea16f79bcfac8099dbfbb4fea8f0a889d4016ccda189f56b6a4c"
    end
  end

  on_linux do
    depends_on "pkg-config" => :build
    depends_on "rust" => :build
    depends_on "fontconfig"
  end

  def install
    if OS.mac? && !build.head?
      # Downloaded as a bare (non-archived) binary named glassy-<arch>-macos;
      # GitHub release assets carry no exec bit, hence the explicit chmod.
      bin.install Dir["glassy-*-macos"].first => "glassy"
      chmod 0755, bin/"glassy"
    else
      system "cargo", "install", *std_cargo_args
    end
  end

  # Install man page if present.
  def post_install
    man1.mkpath
    man1.install "extra/glassy.1" if File.exist?("extra/glassy.1")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/glassy --version 2>&1")
  end
end
