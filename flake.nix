{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  inputs.nci.url = "github:yusdacra/nix-cargo-integration";
  inputs.nci.inputs.nixpkgs.follows = "nixpkgs";
  inputs.parts.url = "github:hercules-ci/flake-parts";
  inputs.parts.inputs.nixpkgs-lib.follows = "nixpkgs";

  outputs = inputs @ {
    parts,
    nci,
    ...
  }:
    parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux"];
      imports = [nci.flakeModule];
      perSystem = {
        pkgs,
        config,
        ...
      }: let
        crateName = "musikquadrupled";
        crateOutputs = config.nci.outputs.${crateName};
      in {
        nci.projects.${crateName}.path = ./.;
        devShells.default = crateOutputs.devShell.overrideAttrs (old: {
          RUST_SRC_PATH = "${config.nci.toolchains.shell}/lib/rustlib/src/rust/library";
          packages =
            (old.packages or [])
            ++ [
              pkgs.rust-analyzer
              (pkgs.writeShellApplication {
                name = "generate-cert";
                runtimeInputs = with pkgs; [mkcert coreutils];
                text = ''
                  mkcert localhost 127.0.0.1 ::1
                  mv localhost+2.pem cert.pem
                  mv localhost+2-key.pem key.pem
                '';
              })
            ];
        });
        packages.default = crateOutputs.packages.release;
      };
    };
}
