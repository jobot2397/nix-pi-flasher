{
  description = "NixOS SD image for Raspberry Pi, configured by Nix Pi Flasher";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # Pure eval: getEnv returns "" and the tracked default is used.
      # Impure eval (the app always passes --impure): an absolute
      # FLASHER_CONFIG path is read as data. The file must not be an
      # untracked relative path — git flakes hide those from Nix.
      configPath =
        let
          fromEnv = builtins.getEnv "FLASHER_CONFIG";
        in
        if fromEnv == "" then
          ./config/default.json
        else if builtins.substring 0 1 fromEnv != "/" then
          throw "FLASHER_CONFIG must be an absolute path, got a relative path"
        else
          /. + fromEnv;

      flasher = builtins.fromJSON (builtins.readFile configPath);
    in
    {
      nixosModules.piImage = ./nix/pi-image.nix;

      packages.aarch64-linux.sdImage =
        let
          system = nixpkgs.lib.nixosSystem {
            system = "aarch64-linux";
            modules = [
              "${nixpkgs}/nixos/modules/installer/sd-card/sd-image-aarch64.nix"
              self.nixosModules.piImage
              {
                piFlasher = {
                  enable = true;
                  setHostname = flasher.setHostname;
                  hostname = flasher.hostname;
                  enableSsh = flasher.enableSsh;
                  createUser = flasher.createUser;
                  username = flasher.username;
                  hashedPassword = flasher.hashedPassword;
                  packages = flasher.packages;
                };
              }
            ];
          };
        in
        system.config.system.build.sdImage;
    };
}
