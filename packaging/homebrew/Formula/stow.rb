# Homebrew formula for Stow — build-from-source (v1, personal/local use).
#
# Why a formula (not a cask): a cask installs a *prebuilt, notarized* artifact.
# v1 is ad-hoc signed and built locally, which avoids the quarantine that would
# otherwise stop the File Provider extension from loading. So we compile on the
# user's machine. UX: `brew tap viraatdas/tap && brew install stow`.
#
# Build-from-source works because Homebrew's build here allows network, so cargo
# (crates.io) and SwiftPM (swift-argument-parser) resolve normally.
#
# When we later add Developer ID + notarization for distribution to other Macs,
# this becomes a Cask pointing at a release on stow.viraat.dev.
class Stow < Formula
  desc "Offload unused files on macOS; rehydrate transparently on access"
  homepage "https://stow.viraat.dev"
  url "https://github.com/viraatdas/stow/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "b5bcea354803224970668ed34c4d028680adf393a8a3684069714a4714f3bd54"
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

    # Build with signing disabled: Apple Silicon still applies an ad-hoc signature
    # so the binaries run. The agent/extension carry restricted entitlements
    # (app-groups, fileprovider) that need a provisioning profile to sign properly
    # — that's wired up with a (free) personal team when the domain is registered.
    #
    # Use `system "xcodebuild"`, NOT Homebrew's `xcodebuild` helper: the helper
    # rewrites the build env and re-triggers the signing/provisioning requirement.
    unsigned = ["CODE_SIGNING_ALLOWED=NO", "CODE_SIGNING_REQUIRED=NO",
                "CODE_SIGN_IDENTITY=", "CODE_SIGN_STYLE=Manual"]
    %w[StowAgent stow].each do |scheme|
      system "xcodebuild", "-project", "Stow.xcodeproj", "-scheme", scheme,
             "-configuration", "Release", "-destination", "platform=macOS,arch=arm64",
             "-derivedDataPath", "build", "build", *unsigned
    end

    products = "build/Build/Products/Release"
    # The faceless agent (with the embedded File Provider extension) lives in libexec.
    libexec.install "#{products}/StowAgent.app"
    bin.install "#{products}/stow"
  end

  # Run the agent in the background; it hosts the extension and the CLI's IPC server.
  service do
    run [opt_libexec/"StowAgent.app/Contents/MacOS/StowAgent"]
    keep_alive true
    log_path var/"log/stow-agent.log"
    error_log_path var/"log/stow-agent.log"
  end

  def caveats
    <<~EOS
      Stow is build-from-source and ad-hoc signed (personal use).
      Start the background agent, then set up:
        brew services start stow
        stow init
    EOS
  end

  test do
    assert_match(/\d+\.\d+\.\d+/, shell_output("#{bin}/stow --version"))
  end
end
