# Homebrew formula for cognitive-memory.
#
# Lives in the user's tap repo (`bhekanik/homebrew-tap` or similar).
# The release workflow uploads tarballs and updates the SHA256 / version
# fields below as part of the tag-driven release.
#
# Until v0.1.0 ships, the version + SHA256 placeholders below are not
# real — fill them in at release time.

class CognitiveMemory < Formula
  desc "Local, always-on memory service for AI agents"
  homepage "https://github.com/bhekanik/cognitive-memory"
  license "MIT OR Apache-2.0"
  head "https://github.com/bhekanik/cognitive-memory.git", branch: "main"

  # Real release artifacts — update at tag time:
  # url "https://github.com/bhekanik/cognitive-memory/releases/download/v0.1.0/cognitive-memory-v0.1.0.tar.gz"
  # sha256 "REPLACE_AT_RELEASE_TIME"
  # version "0.1.0"

  depends_on "rust" => :build

  def install
    cd "cognitive-memory-daemon" do
      system "cargo", "install", *std_cargo_args(path: "crates/daemon")
      system "cargo", "install", *std_cargo_args(path: "crates/cli")
      system "cargo", "install", *std_cargo_args(path: "crates/http-bridge")
    end
  end

  def post_install
    (var/"cognitive-memory").mkpath
    (var/"log/cognitive-memory").mkpath
  end

  service do
    run [opt_bin/"cm-daemon"]
    keep_alive true
    log_path var/"log/cognitive-memory/daemon.log"
    error_log_path var/"log/cognitive-memory/daemon.error.log"
  end

  test do
    assert_match "cm", shell_output("#{bin}/cm --help")
  end
end
