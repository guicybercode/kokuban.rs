cask "kokuban" do
  version "0.4"

  on_arm do
    sha256 "ea777e8a44cb6e51ede5de80b9138cd8c76a4c1400bc9ea67bc45a98ba661703"

    url "https://github.com/guicybercode/kokuban.rs/releases/download/v#{version}/kokuban-v#{version}-aarch64-apple-darwin.dmg"
  end
  on_intel do
    sha256 "9d31b62f0d8cd47294d1cf364c63d8ae2667f538fc8930e81f15e61fc13689a5"

    url "https://github.com/guicybercode/kokuban.rs/releases/download/v#{version}/kokuban-v#{version}-x86_64-apple-darwin.dmg"
  end

  name "Kokuban"
  desc "Native Rust terminal with a chalkboard theme and Kitty and Sixel graphics"
  homepage "https://github.com/guicybercode/kokuban.rs"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on :macos

  app "Kokuban.app"

  zap trash: "~/.config/kokuban"

  caveats <<~EOS
    Kokuban.app is ad-hoc signed, not Developer ID signed or notarized.
    If macOS refuses to open it the first time, use Open from the Finder
    context menu (right-click the app) or clear the quarantine flag:

      xattr -dr com.apple.quarantine /Applications/Kokuban.app
  EOS
end
