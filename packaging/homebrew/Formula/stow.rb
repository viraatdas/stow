# Homebrew formula for Stow — build-from-source.
#
# v0.2.0 ships the working CLI offload engine: `stow init` auto-provisions an S3
# bucket in your own AWS account, `stow offload <file>` uploads + frees disk,
# `stow restore <file>` brings it back byte-identical, `stow status` lists state.
#
# Build-from-source works because Homebrew's build allows network, so cargo
# (crates.io) resolves normally. The CLI is dependency-free Swift + a static Rust
# core, so no provisioning profile is needed.
#
# The transparent Finder-folder (File Provider) layer is a separate, in-progress
# component and is intentionally NOT built/installed here yet.
class Stow < Formula
  desc "Offload unused files on macOS to your own S3, restore on demand"
  homepage "https://stow.viraat.dev"
  url "https://github.com/viraatdas/stow/archive/refs/tags/v0.3.0.tar.gz"
  sha256 "7957f166aa2824cc2c37e77dadf4ac5332bbab82b57b7cb97f10855af72be426"
  license "MIT"
  head "https://github.com/viraatdas/stow.git", branch: "main"

  depends_on "cbindgen" => :build
  depends_on "rust" => :build
  depends_on "xcodegen" => :build
  depends_on arch: :arm64
  depends_on macos: :sonoma # macOS 14+

  def install
    system "xcodegen", "generate"
    system "./scripts/build-rust-xcframework.sh"

    # Build just the CLI. Use `system "xcodebuild"`, NOT Homebrew's xcodebuild
    # helper (the helper rewrites the env and re-triggers signing requirements).
    system "xcodebuild", "-project", "Stow.xcodeproj", "-scheme", "stow",
           "-configuration", "Release", "-destination", "platform=macOS,arch=arm64",
           "-derivedDataPath", "build", "build",
           "CODE_SIGNING_ALLOWED=NO", "CODE_SIGNING_REQUIRED=NO",
           "CODE_SIGN_IDENTITY=", "CODE_SIGN_STYLE=Manual"

    bin.install "build/Build/Products/Release/stow"
  end

  def caveats
    <<~EOS
      Stow stores offloaded files in an S3 bucket in YOUR AWS account, using your
      existing AWS credentials (~/.aws or environment). Get started:
        stow init             # auto-provision the bucket
        stow offload <file>   # upload + free disk space
        stow restore <file>   # bring it back
        stow status           # what's offloaded
    EOS
  end

  test do
    assert_match(/\d+\.\d+\.\d+/, shell_output("#{bin}/stow --version"))
  end
end
