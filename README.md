# Nix Pi Flasher

Desktop app that builds one NixOS SD card image for Raspberry Pi and flashes that image to a removable disk.

The window is a [Tauri 2](https://tauri.app/) app: a React (TypeScript) UI and a Rust backend. The backend does not talk to a Nix channel and does not run `nixos-rebuild`. It runs a locked flake output:

```sh
nix build --impure \
  --extra-experimental-features "nix-command flakes" \
  <flake>#packages.aarch64-linux.sdImage \
  --print-build-logs \
  --out-link <result-symlink>
```

The image is the upstream NixOS aarch64 SD installer module plus the options in `nix/pi-image.nix`. `sdImage.compressImage` is turned off so the build leaves a raw image at `$out/sd-image/<baseName>.img`.

## Boards

Support comes from `nixos/modules/installer/sd-card/sd-image-aarch64.nix` on **nixos-unstable** (pinned in `flake.lock`). That module ships firmware and device trees for Raspberry Pi 3, Pi 4, Pi 400, Pi 5, Pi 500, CM3, CM4, CM5, and Zero 2. It already configures extlinux and the Pi firmware partition; this repo does not add another bootloader. Upstream DHCP and expand-on-boot behavior are left in place.

nixos-unstable is required for Pi 5 firmware. The stable `sd-image-aarch64.nix` line that was current on 2026-11-01 does not include it.

## Prerequisites

- Nix, with the `nix` binary on `PATH`. The app passes `--extra-experimental-features nix-command flakes`, so flakes do not have to be enabled in `nix.conf`.
- An aarch64-linux builder, or binfmt/qemu registered for aarch64. The image is built for `aarch64-linux` even when the app itself runs on x86_64.
- Linux, for listing disks and flashing (`lsblk`, `dd`, and `pkexec` when the app is not root).
- For `npm run tauri dev`: Node, npm, Rust, and the [Tauri Linux dependencies](https://tauri.app/start/prerequisites/) (WebKitGTK 4.1). `mise.toml` installs Node 22 and stable Rust. Nix is not installed by mise.

## Develop

Install [mise](https://mise.jdx.dev/), then from the repo root:

```sh
mise install
npm install
npm run tauri dev
```

If mise asks whether to trust this config, trust it. `mise run dev` is the same as `npm run tauri dev`.

On macOS, the Xcode command-line tools are still required for the linker (`xcode-select --install`). Flashing stays Linux-only.

`npm run build` typechecks the UI and produces the Vite bundle. Rust tests live in `src-tauri`:

```sh
cargo test
cargo check
```

## Settings

`config/default.json` is the config pure evaluation uses. The app writes the same JSON shape to a temp file outside the git tree and passes its absolute path as `FLASHER_CONFIG`. The build always uses `nix build --impure`, which allows `builtins.getEnv` and `builtins.readFile` of that absolute path. With no variable set, the flake reads `./config/default.json`.

```json
{
  "setHostname": true,
  "hostname": "nixpi",
  "enableSsh": true,
  "createUser": false,
  "username": "nixos",
  "hashedPassword": null,
  "packages": []
}
```

Package names and the hostname are JSON data. They are not interpolated into Nix source. If "Create default user" is on, the password is hashed in the app with a glibc SHA-512 crypt (`$6$...`) and only `hashedPassword` is written. The plaintext password is not stored.

When SSH is enabled, `services.openssh` is turned on and `PermitRootLogin` is `no`. The openssh module's default firewall opening is left alone.

Package search is proxied through the Rust backend to search.nixos.org, channel `unstable`, so the names match the pinned nixpkgs. The Elasticsearch username and password are read at runtime from the public page bundle. They are not committed.

## Flash

Flash is enabled only after this process has produced an image. The app lists removable and USB disks with `lsblk`, drops the disk that backs `/`, and will not flash a path that was not in that listing. The confirmation control names the device. Nothing is preselected.

If the process is not root, `pkexec` runs `umount` (a "not mounted" error is ignored) and then:

```sh
dd if=<image> of=<device> bs=4M conv=fsync status=progress
```

The image path is the `.img` from the last successful build. The UI cannot pass a different file. Arguments are passed as an argv array, not through a shell.

## Finding the flake

The backend looks for the flake in this order:

1. `NIX_PI_FLASHER_FLAKE`, when it is set
2. The parent of `CARGO_MANIFEST_DIR` (the repo, for `tauri dev` and a binary built from this tree)

Set `NIX_PI_FLASHER_FLAKE` to the absolute repo path when a packaged binary cannot see the checkout it was built from.
