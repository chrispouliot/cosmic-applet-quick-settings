{
  description = "Development environment for cosmic-applet-quick-settings";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
          };
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "cosmic-applet-quick-settings";
            version = "0.1.0";

            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.lock
                ./Cargo.toml
                ./build.rs
                ./i18n
                ./i18n.toml
                ./resources
                ./src
              ];
            };

            cargoLock = {
              lockFile = ./Cargo.lock;
              allowBuiltinFetchGit = true;
            };

            nativeBuildInputs = with pkgs; [
              libcosmicAppHook
              pkg-config
            ];

            buildInputs = with pkgs; [
              dav1d
              dbus
              expat
              fontconfig
              freetype
              libxcursor
              libinput
              libGL
              libx11
              libxcb
              libxi
              libxkbcommon
              pipewire
              pulseaudio
              stdenv.cc.cc.lib
              udev
              vulkan-loader
              wayland
            ];

            installPhase = ''
              install -Dm0755 target/${pkgs.stdenv.hostPlatform.rust.cargoShortTarget}/release/cosmic-applet-quick-settings \
                 $out/bin/cosmic-applet-quick-settings
              install -Dm0644 target/xdgen/app.desktop \
                $out/share/applications/com.github.chrispouliot.cosmic-applet-quick-settings.desktop
              install -Dm0644 target/xdgen/app.metainfo.xml \
                $out/share/appdata/com.github.chrispouliot.cosmic-applet-quick-settings.metainfo.xml
              install -Dm0644 resources/icon.svg \
                $out/share/icons/hicolor/scalable/apps/com.github.chrispouliot.cosmic-applet-quick-settings.svg
            '';

            meta = with pkgs.lib; {
              description = "A cosmic quick settings applet";
              homepage = "https://github.com/chrispouliot/cosmic-applet-quick-settings";
              license = licenses.gpl3Plus;
              mainProgram = "cosmic-applet-quick-settings";
              platforms = platforms.linux;
            };
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
          };

          runtimeLibraries = with pkgs; [
            wayland
            libGL
            libxkbcommon
            vulkan-loader

            # Quick Settings backends
            pipewire
            udev
            dbus
          ];
        in
        {
          default = pkgs.mkShell {
            # Keep tools in nativeBuildInputs/buildInputs because the local
            # Agent Sandbox / Synth environment consumes this devShell via
            # inputsFrom.
            nativeBuildInputs = with pkgs; [
              # Rust development
              cargo
              rustc
              rust-analyzer
              clippy
              rustfmt

              # COSMIC/libcosmic build tooling
              pkg-config
              cmake
              just
              cargo-generate
              git

              # Useful while developing the combined applet
              bluez
              networkmanager
              wireplumber
              upower
            ];

            buildInputs = with pkgs; [
              # libcosmic / iced / Wayland
              expat
              fontconfig
              freetype
              libxkbcommon
              wayland
              libGL
              vulkan-loader

              # Audio backend
              pipewire

              # Battery / device discovery
              udev

              # D-Bus based NetworkManager / BlueZ / UPower integrations
              dbus

              # Runtime C++ support used by graphical Rust dependencies
              stdenv.cc.cc.lib
            ];

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;

            RUST_BACKTRACE = "1";

            shellHook = ''
              echo "cosmic-applet-quick-settings development shell"
              echo
              echo "Rust:  $(rustc --version)"
              echo "Cargo: $(cargo --version)"
              echo
              echo "Useful commands:"
              echo "  cargo generate gh:pop-os/cosmic-applet-template"
              echo "  cargo build"
              echo "  cargo clippy --all-features"
              echo "  cargo fmt --check"
              echo "  just run"
            '';
          };
        }
      );
    };
}
