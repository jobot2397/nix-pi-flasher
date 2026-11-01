# Image settings for the Nix Pi Flasher SD image.
# Values come from the flake as data (parsed JSON). Package names and
# hostnames are never interpolated into Nix source.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.piFlasher;
in
{
  options.piFlasher = {
    enable = lib.mkEnableOption "Nix Pi Flasher image settings";

    setHostname = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to set networking.hostName from hostname.";
    };

    hostname = lib.mkOption {
      type = lib.types.str;
      default = "nixpi";
      description = "Hostname applied when setHostname is true.";
    };

    enableSsh = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to enable OpenSSH. Root login is disabled. The firewall port follows the openssh module default.";
    };

    createUser = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to create a normal user. Requires hashedPassword.";
    };

    username = lib.mkOption {
      type = lib.types.str;
      default = "nixos";
      description = "Name of the normal user created when createUser is true.";
    };

    hashedPassword = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "glibc SHA-512 crypt hash ($6$...). Plaintext passwords are rejected.";
    };

    packages = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [
        "git"
        "python3Packages.requests"
      ];
      description = "nixpkgs attribute paths to install into environment.systemPackages.";
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      # Fresh images have no previous state to preserve. Pin the version so
      # evaluation does not warn that system.stateVersion is unset.
      system.stateVersion = config.system.nixos.release;

      sdImage.compressImage = false;

      environment.systemPackages = map (
        p:
        let
          parts = lib.splitString "." p;
        in
        if !lib.hasAttrByPath parts pkgs then
          throw "piFlasher.packages: nixpkgs has no attribute path '${p}'"
        else
          let
            pkg = lib.getAttrFromPath parts pkgs;
          in
          if lib.isDerivation pkg then
            pkg
          else
            throw "piFlasher.packages: '${p}' is not a package"
      ) cfg.packages;

      assertions = [
        {
          assertion = !cfg.createUser || (cfg.hashedPassword != null && cfg.hashedPassword != "");
          message = "piFlasher.createUser requires a non-empty hashedPassword (a glibc SHA-512 crypt hash). Plaintext passwords are not accepted.";
        }
      ];
    }

    (lib.mkIf cfg.setHostname {
      networking.hostName = cfg.hostname;
    })

    (lib.mkIf cfg.enableSsh {
      services.openssh = {
        enable = true;
        settings.PermitRootLogin = "no";
      };
    })

    (lib.mkIf cfg.createUser {
      users.groups.networkmanager = { };
      users.users.${cfg.username} = {
        isNormalUser = true;
        extraGroups = [
          "wheel"
          "networkmanager"
        ];
        hashedPassword = cfg.hashedPassword;
      };
    })
  ]);
}
