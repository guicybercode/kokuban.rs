class KokubanRs < Formula
  desc "Native Rust terminal with a chalkboard theme and Kitty and Sixel graphics"
  homepage "https://github.com/guicybercode/kokuban.rs"
  license "BSD-4-Clause"

  livecheck do
    url :homepage
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "https://github.com/guicybercode/kokuban.rs/releases/download/v0.4/kokuban-v0.4-aarch64-apple-darwin.tar.gz"
      sha256 "f440837601ab60a3b4de24a9b3dcd91daf0ab6385941d7c41959d5ff4b03676b"
    end
    on_intel do
      url "https://github.com/guicybercode/kokuban.rs/releases/download/v0.4/kokuban-v0.4-x86_64-apple-darwin.tar.gz"
      sha256 "976d7e092b1928b0c7fdb13ae094af30ba6a450698a6a2199ae017591c0a99a3"
    end
  end

  # The Linux release binary needs glibc 2.39 (Ubuntu 24.04), so Linux builds
  # the tagged source against the host's libraries instead.
  on_linux do
    on_intel do
      url "https://github.com/guicybercode/kokuban.rs/archive/refs/tags/v0.4.tar.gz"
      sha256 "6d6c14df25cb897abae771ba854d4a3ecddcde342d5bbc9deb1bcbdc9a169c9e"
    end

    depends_on "pkgconf" => :build
    depends_on "rust" => :build
    depends_on "fontconfig"
    depends_on "freetype"
  end

  def install
    if OS.linux?
      system "cargo", "install", *std_cargo_args
      (share/"applications").install "assets/io.github.guicybercode.kokuban.desktop"
      (share/"pixmaps").install "assets/kokuban-icon.png" => "io.github.guicybercode.kokuban.png"
      pkgshare.install "kokuban.toml"
      doc.install Dir["*.md"], "CITATION.cff"
      (doc/"docs").install "docs/INSTALL.md", "docs/KEYBINDS.md", "docs/OMARCHY.md",
                           "docs/THIRD_PARTY.md", "docs/third-party-licenses"
    else
      bin.install "kokuban"
      pkgshare.install "kokuban.toml", "assets"
      doc.install Dir["*.md"], "CITATION.cff", "THIRD_PARTY_LICENSES.txt", "THIRD_PARTY_LICENSES", "docs"
      doc.install "THIRD_PARTY_SOURCES" if File.directory?("THIRD_PARTY_SOURCES")
    end
  end

  def caveats
    s = <<~EOS
      Upgrade with `brew upgrade kokuban.rs`. `kokuban --update` is for standalone
      installs and would leave Homebrew's record of the installed version behind.

      An example configuration is in #{opt_pkgshare}/kokuban.toml; copy it to
      ~/.config/kokuban/kokuban.toml to customize Kokuban.
    EOS
    on_macos do
      s += <<~EOS

        The executable is ad-hoc signed, not Developer ID signed or notarized. For a
        Kokuban.app in /Applications install the cask instead: `brew install --cask kokuban`.
      EOS
    end
    on_linux do
      s += <<~EOS

        Kokuban opens the system's X11 or Wayland libraries at run time and needs a
        monospace font. On Ubuntu:
          sudo apt install fonts-dejavu-core libx11-6 libxcursor1 libx11-xcb1 libxi6 libxkbcommon-x11-0
      EOS
    end
    s
  end

  test do
    assert_match "kokuban #{version}", shell_output("#{bin}/kokuban --version")
  end
end
