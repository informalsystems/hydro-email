# System Requirements

### Linux

If on Linux (e.g. Ubuntu), install the essentials:

```bash
sudo apt update && sudo apt install build-essential
```

## Docker

- **MacOS**: `brew install --cask docker`
- **Linux**: `sudo apt -y install docker.io`
  > If prompted, you may need to remove conflicting packages: `sudo apt remove containerd.io`
- **Windows WSL**: [docker desktop wsl](https://docs.docker.com/desktop/wsl/#turn-on-docker-desktop-wsl-2) & `sudo chmod 666 /var/run/docker.sock`
- [Docker Documentation](https://docs.docker.com/get-started/get-docker/)

> **Note:** `sudo` is only used for Docker-related commands in this project. If you prefer not to use sudo with Docker, you can add your user to the Docker group with:
>
> ```bash
> sudo groupadd docker && sudo usermod -aG docker $USER
> ```
>
> After adding yourself to the group, log out and back in for changes to take effect.

> [!NOTE]
> If you are running on a Mac with an ARM chip, you will need to do the following:
>
> - Set up Rosetta: `softwareupdate --install-rosetta`
> - Enable Rosetta (Docker Desktop: Settings -> General -> enable "Use Rosetta for x86_64/amd64 emulation on Apple Silicon")
>
> Configure one of the following networking:
>
> - Docker Desktop: Settings -> Resources -> Network -> 'Enable Host Networking'
> - `brew install chipmk/tap/docker-mac-net-connect && sudo brew services start chipmk/tap/docker-mac-net-connect`

## Docker Compose

- **MacOS**: Already installed with Docker installer
- **Linux + Windows WSL**: `sudo apt-get install docker-compose-v2`
  > If you get a `dpkg` error, you may need to remove the old plugin: `sudo apt remove docker-compose-plugin`
- [Compose Documentation](https://docs.docker.com/compose/)

## Task (Taskfile)

- **MacOS**: `brew install go-task`
- **Linux + Windows WSL**: `npm install -g @go-task/cli`
- [Task Documentation](https://taskfile.dev/)

## JQ

- **MacOS**: `brew install jq`
- **Linux + Windows WSL**: `sudo apt -y install jq`
- [JQ Documentation](https://jqlang.org/download/)

## Rust

```bash docci-ignore
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

rustup toolchain install stable
rustup target add wasm32-wasip2
```

### Upgrade Rust

```bash docci-ignore
# Remove old targets if present
rustup target remove wasm32-wasi || true
rustup target remove wasm32-wasip1 || true

# Update and add required target
rustup update stable
rustup target add wasm32-wasip2
```

## Cargo Components

```bash docci-ignore
# Install required cargo components
# https://github.com/bytecodealliance/cargo-component#installation
cargo install cargo-binstall
cargo binstall cargo-component wasm-tools warg-cli --locked --no-confirm --force

# Install a Warg-compatible version of wkg
# https://github.com/bytecodealliance/wasm-pkg-tools/pull/193
cargo binstall wkg@0.15.1 --locked --no-confirm --force

# Configure default registry
# Found at: $HOME/.config/wasm-pkg/config.toml
wkg config --default-registry wa.dev

# Set default registry for warg as well
warg config --default-registry wa.dev
```

## Http Server

Only needed for oauth. Install any `http-server` command that takes `-p <port>` as an argument (for example, `http-server` from cargo)

## Troubleshooting

On Ubuntu LTS, if you later encounter errors like:

```bash
wkg: /lib/x86_64-linux-gnu/libm.so.6: version `GLIBC_2.38' not found (required by wkg)
wkg: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found (required by wkg)
```

If GLIBC is out of date. Consider updating your system using:

```bash
sudo do-release-upgrade
```
