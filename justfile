prefix := env_var_or_default("HOME", "/root") + "/.local"
share := prefix + "/share"
app_id := "io.github.mozartdiniz.Scrawl"

# Build and run (optionally with an image: `just dev shot.png`)
dev *args:
    cargo run -- {{args}}

# Run with debug logging
run-debug *args:
    RUST_LOG=debug cargo run -- {{args}}

test:
    cargo test

# Install into ~/.local so it shows up in the app grid and "Open With"
install:
    cargo install --path . --root {{prefix}}
    install -Dm644 data/{{app_id}}.desktop -t {{share}}/applications/
    install -Dm644 data/{{app_id}}.metainfo.xml -t {{share}}/metainfo/
    install -Dm644 data/icons/hicolor/scalable/apps/{{app_id}}.svg -t {{share}}/icons/hicolor/scalable/apps/
    -update-desktop-database {{share}}/applications
    -gtk-update-icon-cache -f -t {{share}}/icons/hicolor

uninstall:
    -cargo uninstall scrawl --root {{prefix}}
    rm -f {{share}}/applications/{{app_id}}.desktop
    rm -f {{share}}/metainfo/{{app_id}}.metainfo.xml
    rm -f {{share}}/icons/hicolor/scalable/apps/{{app_id}}.svg
    -update-desktop-database {{share}}/applications

# Flatpak build (needs cargo-sources.json, see the README)
flatpak:
    flatpak-builder --user --install --force-clean build-flatpak {{app_id}}.yaml
