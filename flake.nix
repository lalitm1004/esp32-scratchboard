{
  description = "ESP32-S3 Rust (esp-hal) dev shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux"; # change to aarch64-linux if needed
      pkgs = import nixpkgs { inherit system; };

      # null = latest. Pin e.g. "1.90.0.0" for reproducibility (check espup/esp-rs/rust releases).
      espToolchainVersion = null;
      versionTag = if espToolchainVersion == null then "latest" else espToolchainVersion;
      versionArg = if espToolchainVersion == null then "" else "--toolchain-version ${espToolchainVersion}";

      fhs = pkgs.buildFHSEnv {
        name = "esp-rust-env";

        targetPkgs = p: with p; [
          rustup
          espup
          espflash
          esp-generate
          probe-rs-tools
          just
          gcc
          pkg-config
          openssl
          zlib
          libxml2
          git
          curl
          python3
          cmake
          ninja
        ];

        runScript = "bash";

        profile = ''
          # Toolchain lives outside the project so all ESP projects share it
          export ESP_HOME="$HOME/.local/share/esp-rust"
          export RUSTUP_HOME="$ESP_HOME/rustup"
          export CARGO_HOME="$ESP_HOME/cargo"
          export PATH="$CARGO_HOME/bin:$PATH"
          mkdir -p "$ESP_HOME"

          EXPORT_FILE="$ESP_HOME/export-esp-${versionTag}.sh"

          if [ ! -f "$EXPORT_FILE" ]; then
            echo ">> First run: installing ESP Rust toolchain (this takes a few minutes)..."
            rustup toolchain list | grep -q stable || rustup toolchain install stable --profile minimal
            espup install --targets esp32s3 --export-file "$EXPORT_FILE" ${versionArg} \
              || { rm -f "$EXPORT_FILE"; echo ">> espup failed"; }
          fi

          [ -f "$EXPORT_FILE" ] && . "$EXPORT_FILE"
        '';
      };
    in {
      devShells.${system}.default = fhs.env;
    };
}