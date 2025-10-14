# Additional binaries available for direct download:
# - Linux ARMv7 (32-bit ARM): raygun-1.0.0-linux-armv7.tar.gz
# - Linux x86_64 musl (Alpine/static): raygun-1.0.0-linux-x86_64-musl.tar.gz
# - Linux ARM64 musl (Alpine/static): raygun-1.0.0-linux-aarch64-musl.tar.gz
# Download from: https://github.com/yetidevworks/raygun/releases/download/1.0.0/

class Raygun < Formula
  desc "Raygun CLI"
  homepage "https://github.com/yetidevworks/raygun"
  license :cannot_represent

  on_macos do
    on_arm do
      url "https://github.com/yetidevworks/raygun/releases/download/1.0.0/raygun-1.0.0-darwin-arm64.tar.gz"
      sha256 "d92bd93d1bdb5829aee8e650e0ad6b383a7d7e9a6df98b08ad5e381752a2125e"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/yetidevworks/raygun/releases/download/1.0.0/raygun-1.0.0-linux-x86_64.tar.gz"
      sha256 "f811f93564af0be15b0642d7dd3fde7f8307dcb2ad294addeaa0029bfe0dc3cf"
    end
    on_arm do
      url "https://github.com/yetidevworks/raygun/releases/download/1.0.0/raygun-1.0.0-linux-aarch64.tar.gz"
      sha256 "09845a5411c9c900e1396e8f95423a053bf726bf606883eb40bd3c33b14ae72d"
    end
  end

  def install
    bin.install "raygun"
  end

  test do
    assert_match "raygun", shell_output("#{bin}/raygun --version")
  end
end
