**Français** · [English](#english)

# osd-notify

Petit affichage à l'écran (OSD) pour **Wayland**, écrit en Rust avec `smithay-client-toolkit` et la couche `layer-shell`. Il montre :
- le **volume** (suivi via `pactl`, avec indicateur de sourdine) ;
- des **messages texte** envoyés par un socket Unix : `$XDG_RUNTIME_DIR/osd-notify.sock`.

## Compiler et lancer
```bash
cargo build --release
./target/release/osd-notify &
echo "Bonjour" | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/osd-notify.sock
```
Une ligne envoyée sur le socket = un message affiché.

## Prérequis
- Un compositeur Wayland qui gère `wlr-layer-shell` (KDE Plasma, Sway, Hyprland…).
- `pactl` (PulseAudio ou PipeWire-pulse).
- La police `/usr/share/fonts/google-noto/NotoSans-Bold.ttf` (chemin Fedora/Nobara, constante `FONT` dans `src/main.rs` à adapter sur d'autres distributions).

Le socket est accessible en écriture à tous les utilisateurs locaux (`0666`) : n'importe quel utilisateur de la machine peut afficher un message.

## Licence
[MIT](LICENSE).

---

# English

# osd-notify

A small on-screen display (OSD) for **Wayland**, written in Rust with `smithay-client-toolkit` and the `layer-shell` protocol. It shows:
- the **volume** (tracked through `pactl`, with a mute indicator);
- **text messages** sent through a Unix socket: `$XDG_RUNTIME_DIR/osd-notify.sock`.

## Build and run
```bash
cargo build --release
./target/release/osd-notify &
echo "Hello" | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/osd-notify.sock
```
One line sent to the socket = one message displayed.

## Requirements
- A Wayland compositor that supports `wlr-layer-shell` (KDE Plasma, Sway, Hyprland…).
- `pactl` (PulseAudio or PipeWire-pulse).
- The font `/usr/share/fonts/google-noto/NotoSans-Bold.ttf` (Fedora/Nobara path; adjust the `FONT` constant in `src/main.rs` on other distributions).

The socket is writable by every local user (`0666`): any user on the machine can display a message.

## License
[MIT](LICENSE).
