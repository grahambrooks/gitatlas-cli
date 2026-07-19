class GitatlasCli < Formula
  desc "Multi-repo Git management CLI (companion to the gitatlas GUI)"
  homepage "https://github.com/grahambrooks/gitatlas-cli"
  version "2026.7.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/grahambrooks/gitatlas-cli/releases/download/v2026.7.0/gitatlas-v2026.7.0-aarch64-apple-darwin.tar.gz"
      sha256 "39fcfd7a951990ede3f7181de65cd3cf7eefc621f7203062203db43781c7e6a0"
    end
    on_intel do
      odie "Intel Mac binaries are not provided. Run `cargo install --git https://github.com/grahambrooks/gitatlas-cli --locked` to build from source."
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/grahambrooks/gitatlas-cli/releases/download/v2026.7.0/gitatlas-v2026.7.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "cd51edf3cd96e1e53673ed0f442c677f1e84ba49c5695a936c289066c2185e94"
    end
    on_intel do
      url "https://github.com/grahambrooks/gitatlas-cli/releases/download/v2026.7.0/gitatlas-v2026.7.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "add1d3eafb4a84eef6f7d763748d0f5e90b381b7cb0d2ea22b566970a24eb790"
    end
  end

  def install
    bin.install "gitatlas"
  end

  test do
    assert_path_exists bin/"gitatlas"
  end
end
