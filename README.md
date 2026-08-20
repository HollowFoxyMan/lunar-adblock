# lunar-adblock

Removes advertising from the Lunar Client launcher and the game (the Lunar+ banner and Overwolf/Google ad domains) on Windows.

## How it works

Two independent mechanisms:

1. **Hosts blocking** — Overwolf ad domains (`ads.overwolf.com`, `analyticsnew.overwolf.com`, ...) and Google ad domains (`doubleclick.net`, `googlesyndication.com`, ...) are added to `C:\Windows\System32\drivers\etc\hosts` in a dedicated section. The section self-heals (`self-heal`) if it gets removed or modified.

2. **Launcher patch** — the Lunar+ ad is rendered locally by the launcher itself (not loaded from the network), so hosts blocking cannot remove it. The patch:
   - replaces markers in `dist/assets/use-show-ad.js` inside `resources\app.asar` — the `useShowAd` hook stops rendering the banner;
   - recomputes the SHA-256 of the asar header and replaces it inside `Lunar Client.exe` (Overwolf embeds the expected hash in the binary and verifies it at startup — otherwise the launcher crashes with `Integrity check failed for asar archive`).

## Requirements

- Windows 10/11, run as administrator (the program requests elevation itself)
- Lunar Client installed at `%LOCALAPPDATA%\Programs\Lunar Client` (default install location)

## Usage

```
lunar-adblock.exe
```

Console menu:

```
  status      PROTECTED
  blocking    35 domains   hosts present
  patch       applied   lunar client running (Lunar Client.exe)
  -------------------------------------------------
  l launcher [x] (11)   g game [x] (21)   t telemetry [x] (3)
  h self-heal [x]   a autostart [x]   p patch toggle   q quit   x remove + quit
```

| Key | Action |
|---|---|
| `l` | toggle launcher ad-domain blocking |
| `g` | toggle in-game ad-domain blocking |
| `t` | toggle telemetry blocking |
| `h` | toggle hosts-section self-heal |
| `a` | start on login |
| `p` | apply/remove the launcher patch (closes the launcher, patches `app.asar` + `Lunar Client.exe`, relaunches it) |
| `q` | quit, **tweaks stay active** (hosts and patch keep working) |
| `x` | quit and remove hosts blocking |

## Files

| Path | Purpose |
|---|---|
| `%APPDATA%\lunar-adblock\config` | settings (`launcher_ads`, `game_ads`, `telemetry`, `self_heal`, `launcher_patch`) |
| `%APPDATA%\lunar-adblock\unknown-domains.log` | log of new DNS cache domains not covered by the blocklist (written automatically every 5 seconds) |
| `blocklist.txt` next to the exe | extra domains, one per line |
| `...\Lunar Client\resources\app.asar.lunar-adblock.bak` | backup of the original `app.asar` |
| `...\Lunar Client\Lunar Client.exe.lunar-adblock.bak` | backup of the original `Lunar Client.exe` |

## Launcher updates

When Lunar Client updates, the files are replaced and the patch is lost — the program re-applies it automatically (self-heal, every ~30 seconds while the launcher is closed). Backups are refreshed as well.

## Build & test

```
cargo build --release
cargo test
```

Binary: `target\release\lunar-adblock.exe`.

## Disclaimer

The program does not break the launcher: the patch is an edit of a single UI file (`use-show-ad.js`) plus the matching integrity hash in the exe; login, purchases and updates are unaffected. If anything goes wrong, restore the originals from the backups (or press `p` in the menu).