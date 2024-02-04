# p2p-clipboard

p2p-clipboard is a Peer-to-Peer cross-platform clipboard syncing tool. It enables users to synchronize clipboard contents across multiple machines without the need for a centralized server. 

Currently, it supports Windows, macOS, and partially Linux platforms. While X11 is supported under Linux, please note that **Wayland support is currently broken**.

## Features

- Peer-to-Peer clipboard syncing: Sync clipboard contents across machines seamlessly.
- Cross-platform compatibility: Works on Windows, macOS, and partially on Linux.
- Decentralized and flexible architecture: No need for a centralized server, and works for most network topologies.
- Easy setup and usage: Zero config for basic usage.

## Installation

### Pre-built binaries:

Pre-built binaries for Windows(x86-64), macOS(universal) and Linux(x86-64 and arm64) are available on the [release](./release) page.

macOS users need to do `xattr -d com.apple.quarantine /path/to/p2p-clipboard` first to run it.

### Build from source:

```shell
git clone https://github.com/gnattu/p2p-clipboard.git
cd p2p-clipboard
cargo build --release
```
Please note: if you are using Linux, you will also need `libxcb` and its dev-dependencies installed.

## Usage

```shell
A Peer-to-Peer clipboard syncing tool.

Usage: p2p-clipboard [OPTIONS]

Options:
  -c, --connect <IP:PORT> <PEER_ID>  The remote peer to connect to on boot up
  -k, --key <PATH>                   Path to custom private key. The key should be an ED25519 private key in PEM format
  -l, --listen <IP:PORT>             Local address to listen on
  -p, --psk <PSK>                    Pre-shared key. Only nodes with same key can connect to each other
  -n, --no-mdns                      If set, no mDNS broadcasts will be made
  -h, --help                         Print help
  -V, --version                      Print version

```

### Short Version:

1. Run p2p-clipboard on each machine you want to sync clipboard contents.
2. Ensure that machines are connected to the same network.
3. Copy text to the clipboard on one machine, and it should be synchronized with other machines automatically.

### Long Version:

To synchronize the clipboard, you need at least two nodes connected to each other to form a peer-to-peer network.

p2p-clipboard offers two network bootstrapping modes:

1. **Automatic Discovery:** By default, p2p-clipboard uses mDNS to automatically find peers within the same network. You just run it, and it should discover peers in the same network and start sharing clipboard.

2. **Manual Bootstrapping:** For more complicated networks where mDNS cannot be used, you can manually specify a "boot node" during startup. This node serves as the initial connection point, and it can be any node already in the p2p network. You specifiy it with `-c` or `--connect`, and p2p-clipboard will find all other peers through that peer.

You can manually select which IP or port to use with `-l` or `--listen` option. If you only want to spcify IP and don't care about port number, you use `0` as the port number: `-l 127.0.0.1:0`. If you only want to spcify a port number and want to use all IPs, you use `0.0.0.0` as the IP address: `-l 0.0.0.0:12450`

Each node needs to have a unique keypair as its identifier in the p2p network. It is an `ED25519` keypair and is used for encrypting traffic as well. The `PeerID` is the string representation of the public key of that keypair. By default, the keypair is derived from your machine ID. If you want to specify your own key, you can generate your own private key with the following command:

```shell
openssl genpkey -algorithm Ed25519 -out private_key.pem
```

And then use `-k`or `--key` to use it.

If not everyone in your local network is trusted by you, you can specify an extra pre-shared key to make your p2p network private. The pre-shared key can be any string and is specified with `-p` or `--psk`. Only nodes with the same pre-shared key can be peers with each other. This key won't be sent over network.

If you plan to use p2p-clipboard in a public network, you may want to use `-n` or `--no-mdns` to disable peer discovery with mDNS and manually specify a boot node.

## Limitation

Currently has following limitation:

- Only supports pure text contents.
- The max payload size over network is hardcoded to 64KB after compression at the moment, which is ~150KB raw data. This should be sufficient for most use cases, but it may be increased in the future.
- Wayland support is currently broken, so you will need to use X11 instead. The Wayland standard protocol does not allow windowless applications like p2p-clipboard to access the user clipboard, and compositors need to implement their own protocols for such use cases. wlroots-based compositors and KDE's KWin implement the `wlr_data_control` protocol, but GNOME's Mutter does not.
- The default zero-configuration setup is suitable only when everyone in your local network is trusted by you. While all data is encrypted with TLS, the default setting allows anyone running p2pclipboard in your local network to read your clipboard, potentially exposing sensitive information. **Use a PSK if not everyone in your LAN is trusted.**

## License

This project is licensed under the [MIT License](LICENSE).
