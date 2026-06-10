# Homebrew formula for Stow — build-from-source.
#
# v0.5.0: transparent in-place offloads (opening an offloaded file anywhere on
# disk auto-downloads it — no `stow restore` needed), permanent public share
# links (`stow share` — folders arrive as one zip; `stow unshare` revokes), and
# `stow migrate` to upgrade pre-0.5 stub offloads.
#
# Build-from-source works because Homebrew's build allows network, so cargo
# (crates.io) resolves normally. The CLI is dependency-free Swift + a static Rust
# core, so no provisioning profile is needed.
#
# The CLI is signed (ad-hoc) WITH the App Group entitlement: on macOS 15+ a
# process that opens a shared group container without that entitlement triggers
# the session-only "access data from other apps" TCC prompt on every run. The
# entitlement makes the access silent; no signing certificate is required.
#
# The transparent Finder-folder (File Provider) layer needs Developer-ID signed
# components and is intentionally NOT built/installed here yet.
class Stow < Formula
  desc "Offload unused files on macOS to your own S3, restore on demand"
  homepage "https://stow.viraat.dev"
  url "https://github.com/viraatdas/stow/archive/refs/tags/v0.5.0.tar.gz"
  sha256 "e2890eca85e9f7d8033cb8d2606ee1d3691c591acb974c4662137184acd06b3b"
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

    # Re-sign with a stable identifier + the App Group entitlement (see header).
    system "codesign", "--force", "--options", "runtime",
           "--identifier", "ai.exla.stow.cli",
           "--entitlements", "cli/stow/stow.entitlements",
           "--sign", "-", "build/Build/Products/Release/stow"

    bin.install "build/Build/Products/Release/stow"
  end

  def caveats
    <<~EOS
      Stow stores offloaded files in an S3 bucket in YOUR AWS account, using your
      existing AWS credentials (~/.aws or environment). Get started:
        stow init             # auto-provision the bucket
        stow scan             # dry run: what the policy would offload
        stow schedule         # daily auto-offload + weekly cache cleanup
        stow offload <file>   # upload + free disk space (auto-downloads on open)
        stow share <path>     # permanent public link (folders -> one zip)
        stow restore <file>   # pin it back on disk
        stow clean            # list regenerable caches (--apply to delete)
        stow status           # what's offloaded
    EOS
  end

  test do
    assert_match(/\d+\.\d+\.\d+/, shell_output("#{bin}/stow --version"))
  end
end
